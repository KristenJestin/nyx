# Packaging the nyx Electron shell (Phase 4 — `01KWCJPFP9JSJMRDZGW2E0N904`)

The shell ships as **native installers per OS**, each embedding the matching `nyx-sidecar`
Rust binary OUTSIDE `app.asar` and launching it from the Electron **resources**.

| OS      | Target       | Artifact                                              | Signing                                  |
| ------- | ------------ | ----------------------------------------------------- | ---------------------------------------- |
| Linux   | AppImage     | `release/nyx-<version>.AppImage`                      | none (exec-bit + glibc baseline)         |
| macOS   | DMG + zip    | `release/nyx-<version>.dmg` / `.zip`                  | Developer ID sign + **notarize**         |
| Windows | NSIS         | `release/nyx Setup <version>.exe`                     | Authenticode (env-provided cert)         |

Configuration lives in [`electron-builder.yml`](./electron-builder.yml). The Electron runtime
is **pinned** to `42.4.1` (the POC pin). Never float it.

> Per PDR-0006/0007 the runtime authority is the single **Rust `nyx-sidecar` binary** (no more
> `nyx-napi`/`core-host`). It is spawned as a CHILD of Electron main over a stdin/stdout
> MessagePack pipe. macOS is now a SHIP target (sign + notarize), not a non-goal.

## The hard constraint: the sidecar binary is built per-OS, NOT cross-compiled

`nyx-sidecar` is a native executable for the host OS/arch. **electron-builder does NOT
cross-compile it.** So an artifact for OS _X_ must be built **on OS _X_** (or an _X_ runner /
container), after building the sidecar natively there first:

```sh
cargo build -p nyx-sidecar --release      # → target/release/nyx-sidecar[.exe]
```

`scripts/copy-sidecar.cjs` then stages `target/<profile>/nyx-sidecar[.exe]` into
`dist/sidecar/` **and sets the executable bit (`chmod 0o755`)** — the Linux exec-bit
requirement. `scripts/copy-resources.cjs` stages the bundled Claude plugin into
`dist/resources/claude-plugin/`. Both are part of `bun run build`.

## What ships OUTSIDE the ASAR (and why) — the `asarUnpack` globs

`app.asar` cannot host a file that must be spawned as a native executable or read from a real
path. Two globs are listed under `asarUnpack` in `electron-builder.yml`, so they land in
`app.asar.unpacked/` (preserving the `dist/` prefix) at install time:

- **`dist/sidecar/**`** — the `nyx-sidecar` binary. A native exe CANNOT be spawned from inside
  an asar.
- **`dist/resources/**`** — the bundled Claude plugin. nyx-core reads it from disk
  (`resolve_bundled_plugin_dir → <resource_dir>/resources/claude-plugin`).

### Resource-path resolution in Electron main (production, not just dev)

The bundled main runs from `…/resources/app.asar/dist/main/sidecar.js`. Its
`resolveSidecarBin()` → `deriveSidecarBin(__dirname, app.isPackaged, exe, NYX_SIDECAR_BIN)`
computes `<__dirname>/../sidecar/<exe>` = `app.asar/dist/sidecar/<exe>` and, when
`app.isPackaged`, **redirects `app.asar/ → app.asar.unpacked/`** — so the spawned binary is the
real unpacked file `…/resources/app.asar.unpacked/dist/sidecar/nyx-sidecar[.exe]`. Likewise
`resolveResourceDir()` → `deriveResourceDir(true, process.resourcesPath)` =
`…/resources/app.asar.unpacked/dist`, which the sidecar receives as `NYX_RESOURCE_DIR`. This
holds on all three OS (`process.resourcesPath` is `Foo.app/Contents/Resources` on macOS,
`<install>/resources` on Linux/Windows). The pure `deriveSidecarBin`/`deriveResourceDir`
helpers (in `src/main/sidecar.ts`) make this resolution testable; `scripts/smoke-package.cjs`
asserts both the POSIX and the Windows (`.exe` + `\` separator) branches redirect into
`app.asar.unpacked` and that the resolved POSIX path exists + the binary boots from it.

## Linux — AppImage (produced + verified on the host here)

```sh
cd apps/desktop

# 1. Build the sidecar natively (Linux x64), then the app, then stage + chmod the binary.
cargo build -p nyx-sidecar --release
bun run build

# 2a. FAST de-risk: a throwaway unpacked dir + the embedding/resolution smoke.
bun run package          # electron-builder --dir --linux → release/linux-unpacked/
bun run smoke:package    # asserts sidecar+plugin unpacked OUTSIDE asar, main RESOLVES from
                         # resources, the packaged sidecar boots + streams a PTY

# 2b. The AppImage:
node ../../node_modules/.bun/electron-builder@*/node_modules/electron-builder/cli.js \
  --linux AppImage --config electron-builder.yml
# → release/nyx-<version>.AppImage
chmod +x release/nyx-<version>.AppImage && ./release/nyx-<version>.AppImage   # Wayland-native
```

`smoke:package` is a GATE: it proves the `nyx-sidecar` binary is unpacked at
`resources/app.asar.unpacked/dist/sidecar/`, the Claude plugin is at the resolver path, the
main-process resolver lands on the unpacked binary (POSIX + Windows branches), and the packaged
sidecar handshakes + streams a real PTY — all from the packaged layout.

### Linux — "install" from the source tree (no AppImage, no FUSE)

For a dev/test machine (e.g. Arch, where AppImage needs `fuse2`), the unpacked package IS the
installed app: `release/linux-unpacked/nyx-v2` is a self-contained Electron app dir with the
sidecar + plugin in `resources/app.asar.unpacked/`. `bun run install:linux` (root or
`apps/desktop`) chains everything and registers it as a desktop app:

```sh
bun install
bun run install:linux              # from the repo root
#   1. cargo build -p nyx-sidecar --release
#   2. bun run package              → release/linux-unpacked/nyx-v2  (NYX_SIDECAR_PROFILE=release)
#   3. bun run smoke:package        (skip with --skip-smoke)
#   4. ~/.local/share/applications/nyx-v2.desktop + ~/.local/bin/nyx-v2  (skip with --no-desktop)
```

Both entries point at the absolute path of the binary inside this checkout, so after a
`git pull` re-running the same command rebuilds in place and the launcher keeps working.
Uninstall = delete the `.desktop` file and the symlink. Prerequisites: `bun`, `cargo`, and
`node` on the PATH (the build scripts are plain Node). The launcher uses `build/icon.png`, the
nyx face from nyx-site (`build/icon.svg` is its source; electron-builder reads the same file
for every installer target).

### Linux glibc baseline (portability)

The sidecar dynamically links **glibc** (`libc.so.6`, `libm.so.6`, `libgcc_s.so.1` — `ldd`
shows only these + the loader; SQLite is statically linked). A binary built on a NEW glibc
imports NEW symbol versions and then refuses to run on older systems
(`version 'GLIBC_2.3x' not found`). For a portable AppImage, build the sidecar (and the
Electron app) on the **OLDEST glibc you intend to support** — the documented baseline is
**glibc 2.28 (Debian 10 "buster" / RHEL 8 / Ubuntu 18.04+)**, the common floor for current
Electron 42 on Linux. Practically:

- build the sidecar in a **`debian:bullseye`/`buster`** (or `quay.io/pypa/manylinux_2_28`)
  container, or set a glibc-pinned target;
- verify the floor with `objdump -T target/release/nyx-sidecar | grep -oE 'GLIBC_[0-9.]+' |
  sort -V | tail -1` — it must be **≤ your baseline** (e.g. `GLIBC_2.28`). A build on a bleeding
  host (e.g. Arch glibc 2.43) pulls `GLIBC_2.39` and is NOT portable — that is a build-host
  choice, not a code change.

## macOS — DMG + zip, hardened-runtime SIGNED + NOTARIZED (needs a macOS host + Apple creds)

Configured in `electron-builder.yml` (`mac.target: [dmg, zip]`, `hardenedRuntime: true`,
`notarize: true`, `entitlements: build/entitlements.mac.plist`). The entitlements
(`build/entitlements.mac.plist`) grant the JIT / unsigned-memory / library-validation /
inherit capabilities Electron + the spawned sidecar need under the hardened runtime.

```sh
cd apps/desktop
cargo build -p nyx-sidecar --release      # build on macOS (lipo for universal: x64 + arm64)
bun run build

# Signing + notarization read credentials from the environment:
export CSC_LINK=…                 # path/base64 of the Developer ID Application .p12
export CSC_KEY_PASSWORD=…
export APPLE_ID=…                 # Apple account for notarization
export APPLE_APP_SPECIFIC_PASSWORD=…
export APPLE_TEAM_ID=…
node ../../node_modules/.bun/electron-builder@*/node_modules/electron-builder/cli.js \
  --mac dmg zip --config electron-builder.yml
# → release/nyx-<version>.dmg + .zip  (signed app, unpacked sidecar signed with it, notarized)
```

osx-sign walks `app.asar.unpacked`, so the unpacked `nyx-sidecar` is code-signed with the app
and Gatekeeper admits the spawned native binary. **This step requires a macOS host + a valid
Apple Developer ID + notarization credentials** — none of which exist on the Linux dev host, so
the macOS production + launch is the Phase-5 validation gate.

## Windows — NSIS installer, Authenticode-signed; Job Object kill-with-parent

Configured in `electron-builder.yml` (`win.target: [nsis]`, `win.signtoolOptions`, the NSIS
block). Signing reads `CSC_LINK`/`WIN_CSC_LINK` (the `.pfx`) + `CSC_KEY_PASSWORD` from the
environment (or an EV/Azure Trusted Signing setup).

```powershell
cd apps/desktop
cargo build -p nyx-sidecar --release      # build on Windows x64 → target/release/nyx-sidecar.exe
bun run build

$env:CSC_LINK="…"; $env:CSC_KEY_PASSWORD="…"   # the code-signing cert
node ..\..\node_modules\.bun\electron-builder@*\node_modules\electron-builder\cli.js `
  --win nsis --config electron-builder.yml
# → release\nyx Setup <version>.exe   (signed, with .blockmap + uninstaller)
```

The `nyx-sidecar.exe` sits INSIDE the signed package (`app.asar.unpacked`). **Kill-with-parent
+ descendant reaping** on Windows is a **Job Object**: the sidecar creates a job with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and assigns itself to it at startup
(`crates/nyx-sidecar/src/lifecycle.rs::setup_windows_job_object`, wired in `main.rs`). Every
descendant it spawns inherits the job; when the sidecar's last job handle closes — including an
abrupt parent-death exit (the supervisor closes the pipe → the sidecar's stdin hits EOF → the
run loop exits) — the kernel terminates the WHOLE job, reaping the tree with no orphans. This is
the Windows analog of the Unix process-group reaping. The Job Object code is authored +
dependency-wired (`windows-sys` 0.61, Windows-only) and its API verified against the crate
source, but it **cannot be compiled/run on this Linux host** (no Windows Rust std target) — the
Windows production + launch is the Phase-5 validation gate. **Requires a Windows host + a
code-signing certificate.**

## Reality check (Linux-only dev host)

- ✅ **Implemented + verified here**: the `asarUnpack` of the sidecar, per-OS resource-path
  resolution in Electron main (POSIX + Windows branches asserted), the Linux exec-bit + a
  documented glibc baseline, the macOS sign+notarize config + entitlements wiring, the Windows
  NSIS + signing config, and the Windows Job Object code/config. The **Linux AppImage `--dir`
  package is produced and the sidecar is launched from resources** (`smoke:package` green).
- ⛔ **Cannot be done here** (the Phase-5 validation gate, task `01KWCJPFP9JSJMRDZGW2E0N906`):
  producing + launching the **signed/notarized macOS** app (needs an Apple Developer ID +
  notarization creds + a macOS host) and the **signed Windows** installer (needs a code-signing
  cert + a Windows host), plus the literal "3-OS production + launch OK on each".
