---
title: RPC protocol
summary: How a dekit client talks to a runner, version 1.
related: [rpc/requests, rpc/attach]
hidden: true
---

A client speaks to a runner over a Unix socket, or a named pipe
(`\\.\pipe\dekit-<hash>`) on Windows. The address is in the runner's
record in the runtime directory. This is version 1 of the protocol; it is
not public yet and may still change before the first release.

## Framing

Frozen forever:

```
frame  := len: u32_be, kind: u8, payload: [u8; len - 1]
kind   := 0x00 Ctl (UTF-8 JSON)
        | 0x01 Out (raw bytes, server -> client)
```

The largest frame (`kind` plus payload) is 16 MiB; an oversize or zero
`len` is fatal and the connection closes. An unknown `kind` is skipped.
New kinds may only be sent after the peer advertised a matching feature in
`hello`.

## Control messages

JSON objects tagged by `type`:

```json
{"type":"hello","protocol":1,"version":"0.9.6","app":"dekit 0.9.6","features":[]}
{"type":"request","id":1,"method":"command","params":{"command":"start","target":"services/*"}}
{"type":"response","id":1,"result":{}}
{"type":"response","id":1,"error":{"code":"no_match","message":"..."}}
{"type":"event","name":"input","params":{"Key":{}}}
{"type":"bye","code":"quit"}
```

Decode policy:

- invalid UTF-8 or JSON in a Ctl frame: fatal, close
- unknown `type`: skip, it is a future addition
- malformed body of a known `type`: fatal, close, since the peer has a bug
- unknown request `method`: respond `unknown_method`
- params that do not decode: a request gets `invalid_params`; an event is dropped
- unknown event `name` or unknown fields: ignore

## Handshake

The client sends `hello` first; the server answers with its own `hello`,
or with `bye` (`unsupported_protocol`) and closes. `protocol` is bumped
only for framing or envelope breaks. Evolution is additive (new methods,
events, fields, error codes) and behavior changes ride on `features`.
`version` is the peer's semver, which kernel version resolution reads;
`app` is a human string.

## Compatibility rules

- Additive changes only. A method, event, field, or code is never renamed or reused on the wire.
- Receivers ignore unknown fields and events; servers answer unknown methods with `unknown_method`.
- Behavior changes are gated by `features` in `hello`, not by version bumps.
- Golden fixtures in the dekit source pin the exact encodings and are append-only.
