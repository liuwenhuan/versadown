# Local Tao patch

Base: crates.io `tao 0.35.3`, copied without Cargo cache metadata.
Upstream license: `LICENSE` and `LICENSE.spdx` (unchanged).

Backport: https://github.com/tauri-apps/tao/pull/1252

Only `src/platform_impl/windows/dpi.rs` differs from the published source:
move `GetDC` to the legacy DPI fallback and pair it with `ReleaseDC`.
An additional Windows-only regression test checks that invalid HWNDs take
the documented modern DPI fallback rather than failing at an unnecessary
`GetDC`. This removes redundant work in hit testing and frame calculation;
it is not a claim that all observed resizing latency came from an HDC leak.

Remove this directory and `[patch.crates-io]` when the selected Tauri runtime
supports a Tao release containing the upstream fix. Do not modify the global
Cargo registry cache. Other platforms and dependency versions are unchanged.
