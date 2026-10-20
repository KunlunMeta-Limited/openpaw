# Third-party license notices

This supplement covers the two version-scoped license exceptions proposed in
`deny.toml`. It is not an exhaustive dependency inventory. Retain upstream
notices for all dependencies; distribution review must use the final lockfile.

Distribute this file and `third-party-licenses/` with every released binary or
archive. The T8 packaging check must verify their presence. These upstream
packages are used without source modifications in this change.

| Package | Upstream source archive | License text | SHA-256 of license text |
|---|---|---|---|
| option-ext 0.2.0 (via dirs-sys) | https://static.crates.io/crates/option-ext/option-ext-0.2.0.crate | [MPL-2.0](third-party-licenses/option-ext-0.2.0-MPL-2.0.txt) | `66a3107d5ad6a058aab753eaac2047ccb2ed0e39465dd0fe5844da3e300d5172` |
| webpki-roots 1.0.9 (TLS root data) | https://static.crates.io/crates/webpki-roots/webpki-roots-1.0.9.crate | [CDLA-Permissive-2.0](third-party-licenses/webpki-roots-1.0.9-CDLA-Permissive-2.0.txt) | `e271993808fec50ab29350b39539cdec611a9103f827e0aa26d61da70e2d33f8` |

Source code for the MPL-covered package can be obtained from the exact upstream
source archive linked above. If that covered code is modified or redistributed,
retain its license notices and meet its source availability requirements. The
complete license texts control the applicable terms.

Upgrading either package requires reviewing the actual new license and updating
the version-scoped exception and these notices together. The licenses are not
added to the global allow list and security advisory checks remain enabled.
