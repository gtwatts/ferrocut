# Reused Storytold Rust libraries

Ferrocut retains the core crates of [FilmCraft](https://github.com/storytold/filmcraft)
at `5231852443363f001c3f6b396dd9b1e6461ae2be` (42 packages) and
[EffectCraft](https://github.com/storytold/effectcraft)
at `6943872cf65b3da1275f1e0f808b60b2e51d84dc` (31 packages).
The archive SHA256 values, package inventory, unchanged file hashes and deliberate
omissions are in [manifest.json](manifest.json). Their MIT and Apache license texts,
notices and attribution indexes accompany each copy. One documented FilmCraft
patch fixes a UTF-8 slice in file-URL parsing; original and patched hashes are
recorded in the manifest. All other upstream Rust source is unchanged.
Workspace manifests select the retained core crates; originals are saved
as `Cargo.upstream.toml`.

This is source adoption, not a claim that every retained package is connected,
buildable alone, verified, or equivalent to Adobe software. Ferrocut builds the
dependency closure of its selected adapters. Run `ferrocut capabilities` for the
connection inventory and `ferrocut effects` for usable/unsupported effect controls.
Integration evidence and remaining work are in
[the integration report](../../docs/integrations/STORYTOLD.md).

Desktop `ui-egui`, apps, `xtask`, repository assets, ArtCraft branding and generated
`h264enc/target` oracle outputs are omitted. Source-local fixtures and attribution
sidecars are retained. Several unconnected crates expect excluded build assets or
runtime hosts; copying their source does not enable them.

EffectCraft's own media/render/interchange crates pin FilmCraft
`2c5ba7a72619935f0481f99d59409e0d1844b234`. Those cross-engine dependencies are not
activated or silently repointed to the newer retained FilmCraft snapshot. Enabling
them requires preserving that pin or explicitly testing an upgrade. The connected
EffectCraft effects/path dependency closure does not require those git dependencies.

Verify retained files without network access:

```sh
python3 scripts/storytold-vendor.py check
```
