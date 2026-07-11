// The @pierre/diffs syntax-highlight worker, bundled by Vite via the `?worker` suffix. The worker
// POOL runs Shiki tokenization OFF the main thread so a big diff never blocks the UI; the grammars
// + the Oniguruma WASM are bundled LOCALLY (code-split chunks, no CDN), so the review viewer
// highlights fully OFFLINE — see the `worker: { format: "es" }` note in `vite.config.ts` (PDR-0028).
// The DATA still comes from the sidecar's `repo_diff` bridge — @pierre only RENDERS it.
import DiffsWorker from "@pierre/diffs/worker/worker.js?worker";

/** Create ONE @pierre/diffs Shiki worker (Vite compiles this to a local ES-module worker). */
export const createDiffsWorker = (): Worker => new DiffsWorker();
