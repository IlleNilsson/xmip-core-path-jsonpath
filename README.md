# xmip-core-path-jsonpath

JSONPath path technology: RFC 9535 queries over a JSON Stream — child,
descendant, index, slice, wildcard and filter selectors — the first match read,
every match written, for promote, demote, route and process. A technology of
[xmip-core-path](https://github.com/IlleNilsson/xmip-core-path).

A query is read through `xmip-core-library-codec`'s character reader. Blank
space is the four ASCII characters RFC 9535 names, and a member name takes
every character outside ASCII, as its `name-char` does.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
