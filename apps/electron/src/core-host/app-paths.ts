/**
 * Electron host adapter of FRONTIER 2 — `AppPaths` (the mirror of the Tauri
 * adapter's `resolve_data_dir` + `app.path().resource_dir()`).
 *
 * The core-host runs under `ELECTRON_RUN_AS_NODE`, where `require('electron').app`
 * is UNAVAILABLE (pure Node) — so the host CANNOT call `app.getPath('userData')`
 * itself. Instead, the FULL-Electron main resolves both paths (it has `app`) and
 * passes them in the `HostBootConfig`; this adapter just surfaces them with the
 * same precedence the Tauri side documents.
 *
 *   - `data_dir` — `userData` resolved by main, with `NYX_DATA_DIR` taking
 *     precedence (the e2e harness pins it). The override is honored at the SHELL
 *     layer (main) so a single resolver serves every platform; the host re-checks
 *     `NYX_DATA_DIR` defensively so a direct host spawn (smoke/tests) is correct too.
 *   - `resource_dir` — the read-only bundled-resources dir. In a packaged build this
 *     is the UNPACKED resources path OUTSIDE the asar (a `.node`/plugin can't live
 *     in an asar); `null` in a bare dev/test run.
 *
 * The directory is created if missing, matching the Tauri adapter's
 * `fs::create_dir_all` before `Db::open`.
 */
import fs from "node:fs";

import type { HostBootConfig } from "../shared/host-protocol";

export class ElectronAppPaths {
  private readonly dataDirPath: string;
  private readonly resourceDirPath: string | null;

  constructor(config: HostBootConfig) {
    // `NYX_DATA_DIR` wins even at the host layer (defensive: a direct host spawn in
    // tests sets the env but may not thread it through main's resolution).
    const override = process.env.NYX_DATA_DIR;
    this.dataDirPath = override && override.length > 0 ? override : config.dataDir;
    this.resourceDirPath = config.resourceDir;
  }

  /** The writable per-user data dir, created if missing. */
  dataDir(): string {
    fs.mkdirSync(this.dataDirPath, { recursive: true });
    return this.dataDirPath;
  }

  /** The read-only bundled-resource dir (unpacked, outside asar), or null. */
  resourceDir(): string | null {
    return this.resourceDirPath;
  }
}
