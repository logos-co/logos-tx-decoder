// logos-tx-decoder — offline EVM calldata decoding for a signing surface.
//
// Link against liblogos_tx_decoder.a. Every call is offline: there is no
// network path in this library, so a decode cannot stall an approval.
//
// Strings returned here are owned by the caller and must be released with
// logos_tx_decoder_string_free. All replies are JSON with an `ok` field; a
// failure is reported as {"ok":false,"error":"..."} rather than a null return,
// so a caller that only ever parses JSON stays correct.
//
// No function unwinds, and every one tolerates a NULL handle or NULL string.

#ifndef LOGOS_TX_DECODER_H
#define LOGOS_TX_DECODER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LogosTxDecoder LogosTxDecoder;

// Build a decoder over the embedded ABI database. Returns NULL on failure.
// Worth doing once and keeping: it parses ~430KB of ABI JSON.
LogosTxDecoder *logos_tx_decoder_new(void);

// Safe on NULL.
void logos_tx_decoder_free(LogosTxDecoder *decoder);

// Release any string returned by this library. Safe on NULL.
void logos_tx_decoder_string_free(char *s);

// The call for an approval surface.
//
// `render_lines_json` is the keystore's `render_lines` as a JSON array of
// strings. Transaction legs are recovered from those lines and each is decoded,
// so the interpretation is derived from the exact text being displayed.
//
// Returns {"ok":true,"legs":[{index,chainId,kind,confidence,lines}]}.
// An empty `legs` is NOT an error — it means nothing decodable was found, and
// the caller should show the verbatim lines alone.
//
// `confidence` is "verified" only when the called address is itself in the
// database and declares the selector. "signature_only" is a guess from 4 bytes
// and proves nothing about the contract.
char *logos_tx_decoder_describe_render_lines(LogosTxDecoder *decoder,
                                             const char *render_lines_json);

// Decode one call from structured fields. `to` may be "" for a contract
// creation; `data` may be "" or "0x"-prefixed hex. Returns the full decode plus
// a rendered `lines` array.
char *logos_tx_decoder_decode_call(LogosTxDecoder *decoder, uint64_t chain_id,
                                   const char *to, const char *data);

// Replace the token registry: what an ADDRESS is called, and in what units it
// counts. `json` is a token list document ({"tokens":[...]}) or a
// token_list_module reply forwarded verbatim. The decoder ships a snapshot, so
// this is only needed to hand it a newer or different list.
//
// Returns {"ok":true,tokens,chains}. On a parse failure the previous registry
// is kept, so a bad list costs the naming rather than the decoder.
//
// A registry NAMES addresses and supplies decimals. It cannot raise
// `confidence`: naming a token says nothing about what its code does, so a list
// a user can add to cannot make hostile calldata look checked. The rendered
// line says which list answered.
char *logos_tx_decoder_set_token_list(LogosTxDecoder *decoder, const char *json);

// {"ok":true,schema,source,upstreamRev,generated,contracts,functions}
char *logos_tx_decoder_db_status(LogosTxDecoder *decoder);

#ifdef __cplusplus
}
#endif

#endif  // LOGOS_TX_DECODER_H
