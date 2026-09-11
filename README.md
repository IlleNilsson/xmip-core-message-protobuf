# xmip-core-message-protobuf

Protobuf shape: a wire-format message as one part per top-level field named by its number with the wire type as the media type suffix, or a length-delimited stream as one part per message; the type_url of an Any-shaped first field as the announced type. A technology of [xmip-core-message](https://github.com/IlleNilsson/xmip-core-message).

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
