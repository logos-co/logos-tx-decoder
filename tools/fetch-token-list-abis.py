#!/usr/bin/env python3
"""Fetch source-verified ABIs for every EVM token in a Uniswap token list.

The output is a deterministic snapshot consumed by ``build-abi-db.py``.  Every
valid EVM token is kept even when no source-verified ABI can be found: the DB
builder associates that address with its bundled standard ERC-20 interface and
marks those selector matches as list-derived rather than verified.

Sourcify is queried first because it is public, multi-chain and needs no key.
When ``ETHERSCAN_API_KEY`` is set, Etherscan V2 is used as a fallback.  Responses
are cached per ``(chain, address)`` so interrupted refreshes resume cheaply.

    ./tools/fetch-token-list-abis.py https://tokens.uniswap.org/ \
        --output /tmp/uniswap-token-abis.json \
        --cache-dir /tmp/uniswap-token-abi-cache
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter
from datetime import date
from pathlib import Path


SCHEMA = 1
SOURCIFY = "https://sourcify.dev/server/v2/contract"
ETHERSCAN = "https://api.etherscan.io/v2/api"
ADDRESS = re.compile(r"^0x[0-9a-fA-F]{40}$")
USER_AGENT = "logos-tx-decoder ABI snapshot builder"


def read_bytes(location):
    if location.startswith(("https://", "http://")):
        request = urllib.request.Request(location, headers={"User-Agent": USER_AGENT})
        with urllib.request.urlopen(request, timeout=120) as response:
            return response.read(), response.headers.get("ETag", "")
    return Path(location).read_bytes(), ""


def get_json(url, timeout):
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.load(response)


def useful_abi(value):
    return isinstance(value, list) and any(
        isinstance(item, dict) and item.get("type") == "function" for item in value
    )


def sourcify_abi(chain, address, timeout):
    url = f"{SOURCIFY}/{chain}/{address}?fields=abi"
    try:
        body = get_json(url, timeout)
    except urllib.error.HTTPError as error:
        # 400 is Sourcify's answer for a chain it does not serve (for example
        # retired Kovan); 404 is an unverified address on a supported chain.
        if error.code in (400, 404):
            return None
        raise
    abi = body.get("abi") if isinstance(body, dict) else None
    return abi if useful_abi(abi) else None


def etherscan_abi(chain, address, api_key, timeout):
    query = urllib.parse.urlencode({
        "chainid": chain,
        "module": "contract",
        "action": "getabi",
        "address": address,
        "apikey": api_key,
    })
    body = get_json(f"{ETHERSCAN}?{query}", timeout)
    if not isinstance(body, dict) or body.get("status") != "1":
        return None
    try:
        abi = json.loads(body.get("result", ""))
    except (TypeError, json.JSONDecodeError):
        return None
    return abi if useful_abi(abi) else None


def cache_path(cache_dir, chain, address):
    return cache_dir / str(chain) / f"{address.lower()}.json"


def cached(cache_dir, chain, address):
    if cache_dir is None:
        return None
    path = cache_path(cache_dir, chain, address)
    if not path.exists():
        return None
    try:
        body = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None
    if body.get("state") not in ("found", "missing"):
        return None
    return body


def store_cache(cache_dir, chain, address, body):
    if cache_dir is None:
        return
    path = cache_path(cache_dir, chain, address)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(body, separators=(",", ":")))


def fetch_one(token, cache_dir, api_key, timeout, retries, offline):
    chain, address = token["chainId"], token["address"]
    provider_names = ["sourcify"] + (["etherscan-v2"] if api_key else [])
    hit = cached(cache_dir, chain, address)
    if hit is not None:
        # A later run with an Etherscan key must retry a Sourcify-only miss.
        # Legacy cache entries from the first version of this tool implicitly
        # record that Sourcify completed.
        tried = hit.get("providers", ["sourcify"] if hit.get("state") == "missing" else [])
        if hit.get("state") == "found" or set(provider_names).issubset(tried):
            return token, hit, True
    if offline:
        body = {"state": "missing", "source": "offline-cache-miss"}
        return token, body, False

    providers = [("sourcify", lambda: sourcify_abi(chain, address, timeout))]
    if api_key:
        providers.append(("etherscan-v2", lambda: etherscan_abi(
            chain, address, api_key, timeout)))

    errors, completed = [], []
    for source, fetch in providers:
        for attempt in range(retries + 1):
            try:
                abi = fetch()
                if abi is not None:
                    body = {"state": "found", "source": source, "abi": abi}
                    store_cache(cache_dir, chain, address, body)
                    return token, body, False
                completed.append(source)
                break
            except (OSError, ValueError, json.JSONDecodeError) as error:
                errors.append(f"{source}: {type(error).__name__}: {error}")
                if attempt < retries:
                    time.sleep(0.5 * (attempt + 1))

    body = {
        "state": "missing",
        "source": "erc20-standard",
        "providers": completed,
    }
    if errors:
        body["errors"] = errors
    # Do not make a transient error permanent. Completed 404/no-ABI answers
    # are safe to cache; a failed provider is retried on the next refresh.
    if set(provider_names).issubset(completed):
        store_cache(cache_dir, chain, address, body)
    return token, body, False


def normalize_tokens(document):
    if not isinstance(document, dict) or not isinstance(document.get("tokens"), list):
        raise SystemExit("token list must be an object with a `tokens` array")

    tokens, skipped, seen = [], [], set()
    for position, raw in enumerate(document["tokens"]):
        if not isinstance(raw, dict):
            skipped.append({"index": position, "reason": "not an object"})
            continue
        address = raw.get("address")
        chain = raw.get("chainId")
        if (not isinstance(chain, int) or isinstance(chain, bool)
                or not 0 < chain <= (2**64 - 1)
                or not isinstance(address, str) or not ADDRESS.fullmatch(address)):
            skipped.append({
                "index": position,
                "chainId": chain,
                "address": address,
                "reason": "not an EVM (chainId, 20-byte address) entry",
            })
            continue
        identity = (chain, address.lower())
        if identity in seen:
            skipped.append({
                "index": position,
                "chainId": chain,
                "address": address,
                "reason": "duplicate EVM identity",
            })
            continue
        decimals = raw.get("decimals")
        if not isinstance(decimals, int) or not 0 <= decimals <= 255:
            skipped.append({
                "index": position,
                "chainId": chain,
                "address": address,
                "reason": "decimals is not an integer from 0 through 255",
            })
            continue
        seen.add(identity)
        tokens.append({
            "chainId": chain,
            "address": address.lower(),
            "name": str(raw.get("name") or raw.get("symbol") or address),
            "symbol": str(raw.get("symbol") or raw.get("name") or address),
            "decimals": decimals,
        })
    tokens.sort(key=lambda token: (token["chainId"], token["address"]))
    return tokens, skipped


def list_metadata(document, source, blob, etag, total, evm, skipped):
    version = document.get("version") or {}
    version_text = ".".join(str(version.get(k, 0)) for k in ("major", "minor", "patch"))
    return {
        "source": source,
        "sha256": hashlib.sha256(blob).hexdigest(),
        "etag": etag.strip('"'),
        "name": document.get("name", ""),
        "timestamp": document.get("timestamp", ""),
        "version": version_text,
        "tokens": total,
        "evmTokens": evm,
        "skippedTokens": skipped,
    }


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("token_list", help="Uniswap token-list JSON file or http(s) URL")
    parser.add_argument("--output", required=True, help="where to write the ABI snapshot")
    parser.add_argument("--cache-dir", help="persistent per-contract response cache")
    parser.add_argument("--jobs", type=int, default=12, help="concurrent lookups (default: 12)")
    parser.add_argument("--timeout", type=int, default=30, help="seconds per request")
    parser.add_argument("--retries", type=int, default=2, help="retries after transient errors")
    parser.add_argument("--offline", action="store_true", help="read cache only")
    parser.add_argument(
        "--etherscan-key-env", default="ETHERSCAN_API_KEY",
        help="environment variable holding an optional Etherscan V2 key")
    args = parser.parse_args()
    if args.jobs < 1 or args.timeout < 1 or args.retries < 0:
        parser.error("jobs and timeout must be positive; retries cannot be negative")

    blob, etag = read_bytes(args.token_list)
    try:
        document = json.loads(blob)
    except json.JSONDecodeError as error:
        raise SystemExit(f"token list is not JSON: {error}") from error
    tokens, skipped = normalize_tokens(document)
    print(f"read {len(document['tokens'])} entries: {len(tokens)} EVM tokens, "
          f"{len(skipped)} skipped", file=sys.stderr)

    cache_dir = Path(args.cache_dir) if args.cache_dir else None
    api_key = os.environ.get(args.etherscan_key_env, "")
    results = []
    cache_hits = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as executor:
        futures = [executor.submit(
            fetch_one, token, cache_dir, api_key, args.timeout, args.retries, args.offline)
            for token in tokens]
        for done, future in enumerate(concurrent.futures.as_completed(futures), 1):
            token, result, was_cached = future.result()
            results.append((token, result))
            cache_hits += int(was_cached)
            if done % 50 == 0 or done == len(futures):
                print(f"looked up {done}/{len(futures)}", file=sys.stderr)

    # Deduplicate identical verified ABIs. The DB builder will deduplicate their
    # functions once more across contracts.
    abis, abi_index = [], {}
    rows = []
    source_counts = Counter()
    for token, result in sorted(results, key=lambda item: (
            item[0]["chainId"], item[0]["address"])):
        source = result["source"]
        source_counts[source] += 1
        row = dict(token)
        row["abiSource"] = source
        if result["state"] == "found":
            encoded = json.dumps(result["abi"], sort_keys=True, separators=(",", ":"))
            index = abi_index.get(encoded)
            if index is None:
                index = len(abis)
                abi_index[encoded] = index
                abis.append(result["abi"])
            row["abi"] = index
        rows.append(row)

    snapshot = {
        "schema": SCHEMA,
        "generated": date.today().isoformat(),
        "tokenList": list_metadata(
            document, args.token_list, blob, etag, len(document["tokens"]),
            len(tokens), len(skipped)),
        "providers": [SOURCIFY] + ([ETHERSCAN] if api_key else []),
        "stats": {
            "uniqueVerifiedAbis": len(abis),
            "abiSources": dict(sorted(source_counts.items())),
        },
        "abis": abis,
        "tokens": rows,
        "skipped": skipped,
    }
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(snapshot, separators=(",", ":")))
    print(f"wrote {output} ({output.stat().st_size} bytes): "
          f"{dict(sorted(source_counts.items()))}; {cache_hits} cache hits", file=sys.stderr)


if __name__ == "__main__":
    main()
