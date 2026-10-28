# Vendored libghostty-vt

Three slices, one per host platform — `macos-universal/` (fat arm64 +
x86_64), `linux-aarch64/`, and `linux-x86_64/`. `build.rs` picks the slice
from the target triple, so a headless Linux host parses PTY output with the
exact same VT engine as the Mac app and the phone. The Linux slices are
cross-compiled from macOS by zig (`build.sh` builds all three); they come
from the generic static path in ghostty's `build.zig`, not the xcframework,
and link against `libstdc++` rather than `libc++`.

`macos-universal/libghostty-vt.a` is the standalone terminal-emulation C
library from ghostty (https://libghostty.tip.ghostty.org), used by
`src/terminal_viewport.rs` so the host parses PTY output with the exact same
VT engine that renders it on desktop and phone (GhosttyKit). The archive is
a fat arm64 + x86_64 static lib with the vendored SIMD deps (simdutf,
highway) combined in, so it links with no extra dependencies beyond libc++.

Built from ghostty commit `6ba144158577d033ccb6ed4b574e4013957c9161` (tip,
2026-10-27). The Mac app's GhosttyKit.xcframework is still built from the
older `2da015cd6` (2026-07-06; its own vendor tree under
`clients/native/vendor/libghostty-spm/`), so Host and renderer are on
different ghostty commits until that build is moved too. The Host only
emits standard VT to the renderer (snapshots, replay), so the skew is
safe. Rebuild with:

```sh
git clone https://github.com/ghostty-org/ghostty /tmp/ghostty && \
  git -C /tmp/ghostty checkout 6ba144158577d033ccb6ed4b574e4013957c9161
ZIG=/path/to/zig-0.17.0/zig ./build.sh --ghostty /tmp/ghostty   # or GHOSTTY_SRC=…
```

Requirements:

- zig 0.17.0 (ghostty's `build.zig.zon` enforces the minimum). The
  ziglang.org release tarball links fine on macOS 26; Homebrew's `zig`
  works once it is on 0.17.
- Xcode Metal Toolchain is NOT needed for lib-vt (no renderer).

The C API is declared by hand in `src/ghostty_vt.rs` (no bindgen). If you
bump the vendored archive, diff `include/ghostty/vt/*.h` in the ghostty
checkout against the declarations there — the `layout_matches_type_json`
test cross-checks struct sizes against `ghostty_type_json()` (its `types`
map) at runtime. The 2026-10 bump crossed two ABI breaks: scrollback moved
from `GhosttyTerminalOptions.max_scrollback` to
`ghostty_terminal_set(GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES)`, and
`ghostty_terminal_mode_get` became `ghostty_terminal_get(…DATA_MODE…)` with
a `GhosttyTerminalModeConfig`.

## Unpeel source patches (`patches/`)

`build.sh` applies every `patches/*.patch` to the checkout before `zig build`
and reverts them on exit (even on failure), so the checkout is never left
modified. Today there is one:

- `0001-unpeel-small-pages.patch` — `std_capacity` 215×215 → 215×32 cells
  and 8 KiB → 2 KiB grapheme bytes. Why: `PageList.minMaxSize` forces at
  least two standard pages, so upstream's page size silently raises any
  scrollback byte budget under ~1 MiB to that floor (the Host's
  filled-terminal footprint). Rationale and the measurement recipe: the
  private "pty-core" design record "Round 3, Lane 3". The patched constants
  do not change any C API type, so `layout_matches_type_json` still
  applies.

Dropped in the 2026-10 bump because upstream now covers them: the
`page_preheat = 0` half of 0001 (the page pool is demand-paged, so preheat
costs address space only) and `0002-unpeel-pack-page-metadata.patch`
(pages initialize from zeroed memory). `vt_footprint_per_terminal` on an
80×24 Host terminal with the 256 KiB budget, before → after:

| | 2da015cd + 0001/0002 | 6ba144158 unpatched | 6ba144158 + 0001 |
| --- | --- | --- | --- |
| per empty terminal | 61 KiB | 20 KiB | 20 KiB |
| per filled terminal (10k lines) | 286 KiB | 778 KiB | 225 KiB |
| feed throughput | 357 MiB/s | 493 MiB/s | 735 MiB/s |

`GHOSTTY_SRC=/path` builds from another copy of the checkout. `SLICES=macos`
rebuilds only the universal macOS archive; the default rebuilds all three.
All three slices were last rebuilt together on 2026-10-28 from ghostty
`6ba144158577d033ccb6ed4b574e4013957c9161` with
`0001-unpeel-small-pages.patch` applied (`build.sh`, default `SLICES`, zig
0.17.0 cross-building the Linux slices from macOS).
