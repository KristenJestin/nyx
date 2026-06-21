# Packaging the nyx Electron shell (task #20)

This shell ships as **native installers per OS**:

| OS      | Target     | Artifact                              |
| ------- | ---------- | ------------------------------------- |
| Windows | NSIS       | `release/nyx Setup <version>.exe`     |
| Linux   | AppImage   | `release/nyx-<version>.AppImage`      |

Configuration lives in [`electron-builder.yml`](./electron-builder.yml). The runtime
is **pinned** to Electron `42.4.1` (the POC pin; matches the napi ABI `146`). Never
float it.

> macOS / mobile are explicit **non-goals**. There is no sidecar fallback — the
> native `.node` is the only core path.

## The hard constraint: native `.node` is built per-OS, NOT cross-compiled

`nyx-napi` is a native Node addon (`*.node`) compiled for the host OS/arch. **electron-builder
does NOT cross-compile it.** So an artifact for OS _X_ must be built **on OS _X_** (or
an _X_ runner / container), after building the `.node` natively there first:

```sh
bun run --filter @nyx/napi build     # produces crates/nyx-napi/nyx-napi.<platform>.node
```

`scripts/copy-napi.cjs` then stages whatever `.node` exists into `dist/native/`, and
`scripts/copy-resources.cjs` stages the bundled Claude plugin into
`dist/resources/claude-plugin/`. Both are part of `bun run build`.

## What ships outside the ASAR (and why)

`app.asar` cannot host files that must be `mmap`'d or read from a real path:

- **the native addon** (`dist/native/**`) — a `.node` cannot be loaded from inside an asar;
- **the core-host entry** (`dist/core-host/**`) — spawned as a real file via
  `ELECTRON_RUN_AS_NODE`;
- **the bundled Claude plugin** (`dist/resources/**`) — nyx-core reads it from disk
  (`resolve_bundled_plugin_dir` → `<resource_dir>/resources/claude-plugin`).

These three globs are listed under `asarUnpack` in `electron-builder.yml`, so they
land in `app.asar.unpacked/` at install time. The main process resolves the unpacked
resource root as `…/resources/app.asar.unpacked/dist` and passes it to the core-host
as `resourceDir` (see `src/main/core-host.ts → resolveResourceDir`), so the host finds
the `.node` **and** the Claude plugin **from the installation, with no source path**.

## Windows (exercisable here, and exercised)

```powershell
cd apps/electron

# 1. Build the .node natively (Windows x64), then the app.
bun run --filter @nyx/napi build
bun run build

# 2a. FAST de-risk: a throwaway unpacked dir + the embedding smoke.
bun run package          # electron-builder --dir --win  → release/win-unpacked/
bun run smoke:package    # asserts .node + Claude plugin unpacked, host boots, ConPTY streams

# 2b. The real installer.
node ../../node_modules/.bun/electron-builder@*/node_modules/electron-builder/cli.js \
  --win nsis --config electron-builder.yml
# → release/nyx Setup <version>.exe   (signed, with .blockmap + uninstaller)
```

`smoke:package` is a GATE: it launches the **packaged** `nyx.exe` as the
`ELECTRON_RUN_AS_NODE` host, proves the `.node` and the Claude plugin are unpacked at
the exact resolver path, that the host boots `ready` (nyx-core + ABI + nodePure), the
MCP server starts, and a real ConPTY streams output — all from the packaged layout.

## Linux AppImage (configured here; BUILD + VALIDATION deferred to the phase-7 gate)

The Linux target is **fully configured** in `electron-builder.yml`
(`linux.target: AppImage`, `category: Development`, the same `asarUnpack` globs). It
is **NOT buildable on this Windows host**: electron-builder cannot cross-compile the
Linux `.node`, and there is no Linux machine in this session (per user decision the
Linux/Wayland validation is the phase-7 gate). Do not fabricate a Linux build here.

**On a Linux x64 host** (or a Linux CI runner / container), the build is:

```sh
cd apps/electron

# 1. Build the Linux .node natively, then the app.
bun run --filter @nyx/napi build      # → crates/nyx-napi/nyx-napi.linux-x64-gnu.node
bun run build

# 2a. (optional) the same fast embedding smoke, on Linux:
bun run package          # electron-builder --dir --linux → release/linux-unpacked/
bun run smoke:package    # proves the .node + Claude plugin + portable-pty on Linux

# 2b. The AppImage:
node ../../node_modules/.bun/electron-builder@*/node_modules/electron-builder/cli.js \
  --linux AppImage --config electron-builder.yml
# → release/nyx-<version>.AppImage
```

Then validate natively under **Wayland** (the PRD's Linux contract):

```sh
chmod +x "release/nyx-<version>.AppImage"
./release/nyx-<version>.AppImage           # launches the Wayland-native window
```

The Wayland/HiDPI command-line flags are already applied pre-`app.ready`
(`src/main/wayland.ts`, verified by `smoke:wayland-flags`). The phase-7 gate is the
session that builds the AppImage on Linux and exercises PTY / DB / MCP / Claude
plugin + the Wayland/HiDPI surface natively.

### Linux build prerequisites (documented for the gate)

- a Linux **x64** host (or container) with the build toolchain for `nyx-napi`
  (Rust + cargo, a C toolchain);
- **bun** (the repo's package manager);
- electron-builder pulls the AppImage tooling (`appimagetool`) automatically on first
  Linux build; on a minimal container you may need `libfuse2` to _run_ the produced
  AppImage (building it does not require FUSE).
