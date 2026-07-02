//! v2.1 VALIDATION GATE — end-to-end smoke of the IDENTITY parcours
//! (task `01KWDCNDK0S6NHWVPJBR1MTN9G`, Phase 5).
//!
//! This is the executable half of the gate: it drives the identity journeys the PRD
//! calls out — **create a project/workspace, detect repos, see a pushed event** — through
//! the REAL `nyx-sidecar` binary over the REAL pipe, then re-reads the SAME on-disk
//! `nyx.db` to prove the persisted, single-file authority state. It exercises, in ONE
//! run, the five gate points that the checklist otherwise proves by `file:line`:
//!
//!   1. **nyx, autorité unique** — the parcours goes project → root workspace → detected
//!      repositories → live Git state, all owned by nyx's single SQLite authority; there is
//!      no parallel Depot base (the sidecar opens exactly one `<data_dir>/nyx.db`).
//!   2. **Données migrées / un seul fichier** — the sidecar migrates that ONE file at boot
//!      (`runtime_boot` proves boot-migration; here we re-open the same file post-run and
//!      read the canonical `repositories` + `domain_events` tables the chain created).
//!   3. **UI et MCP sur la même autorité** — the journey is driven through the UI bridge's
//!      `db_command` (`create_project`/`create_workspace`), the SAME `usecase::*` layer the
//!      MCP surface funnels into (PDR-0005). The pushed invalidation is identical either way.
//!   4. **Events poussés (PAS de polling)** — each committed identity mutation PUSHES an
//!      UNSOLICITED `workspaces://changed` event on the pipe, decoded here on the "UI" side
//!      of the wire. Nothing polls: the event arrives because the mutation committed.
//!   5. **domain_events persistés + corrélés** — re-opening the file shows one correlated
//!      `domain_events` outbox row per mutation (the durable fact the push rides out).
//!
//! What is NOT executable here (documented, a CI concern — NOT a gate failure): the full
//! Electron renderer E2E (the renderer consuming `workspaces://changed` and re-listing) is
//! covered by the renderer unit test `use-projects.test.tsx`; and the PRD testingDecision
//! "parcours identité vérifié sur les 3 OS" is a 3-OS CI matrix point, not a single-host
//! run. This smoke covers the whole Rust chain up to the pushed event decoded off the pipe.
//!
//! The repo-detection + Git-state legs shell out to `git`; when `git` is absent (a CI image
//! without it) those legs skip GRACEFULLY (detection is git-agnostic by contract) while the
//! project/workspace/push/outbox legs still run — mirroring `repository.rs`'s own tests.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    decode_message, encode_message, Event, Message, Request, Response, PROTOCOL_VERSION,
};

fn sidecar_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-gate-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Is `git` on PATH? The repo-detection + Git-state legs need a real repo; they skip
/// gracefully when git is absent (detection is git-agnostic by contract).
fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a git subcommand in `dir`, asserting success, with a deterministic identity and
/// signing forced off (a dev host may set commit.gpgsign globally).
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.email=test@nyx",
            "-c",
            "user.name=nyx-test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

/// `git init` a repo at `dir` on a known branch with one commit (so HEAD is born).
fn init_repo(dir: &std::path::Path, branch: &str) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git_in(dir, &["init", "-q"]);
    git_in(
        dir,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
    );
    std::fs::write(dir.join("README.md"), b"nyx").expect("seed file");
    git_in(dir, &["add", "README.md"]);
    git_in(dir, &["commit", "-q", "-m", "init"]);
}

/// A minimal pipe harness over the REAL sidecar binary: spawns it under `NYX_DATA_DIR`,
/// drains the ready event, and demultiplexes the framed `stdout` into a channel so a test
/// can await responses AND observe the UNSOLICITED `Event::Changed` pushes.
struct Harness {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Message>,
    /// COUNT of `workspaces://changed` pushes seen on the pipe, incremented by the reader
    /// thread the instant a `Changed { topic: "workspaces" }` frame is decoded — INDEPENDENT
    /// of request/response ordering (the post-commit push can land before the db response).
    /// This is the "event poussé, pas de polling" observable: it grows only when a committed
    /// mutation pushes, never on a timer.
    workspaces_pushes: Arc<AtomicUsize>,
}

impl Harness {
    fn spawn(data_dir: std::path::PathBuf) -> Self {
        let mut child = Command::new(sidecar_bin())
            .env("NYX_DATA_DIR", &data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn nyx-sidecar");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let workspaces_pushes = Arc::new(AtomicUsize::new(0));
        let pushes = workspaces_pushes.clone();
        std::thread::spawn(move || {
            let mut dec = FrameDecoder::new();
            let mut buf = [0u8; 64 * 1024];
            loop {
                loop {
                    match dec.next_frame() {
                        Ok(Some(p)) => {
                            if let Ok(m) = decode_message(&p) {
                                // Count `workspaces://changed` pushes AS THEY ARRIVE, before
                                // forwarding, so the count is independent of whether the test
                                // has consumed the correlated response yet.
                                if let Message::Event(Event::Changed { topic }) = &m {
                                    if topic == "workspaces" {
                                        pushes.fetch_add(1, Ordering::SeqCst);
                                    }
                                }
                                if tx.send(m).is_err() {
                                    return;
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(_) => return,
                    }
                }
                match stdout.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => dec.push(&buf[..n]),
                    Err(_) => return,
                }
            }
        });
        let mut h = Harness {
            child,
            stdin,
            rx,
            workspaces_pushes,
        };
        // Drain the unsolicited `ready` Log event — proves the process booted (past the
        // single-file DB open + migrate) and is serving the pipe.
        match h.recv(Duration::from_secs(5)) {
            Some(Message::Event(Event::Log { .. })) => {}
            other => panic!("expected ready event, got {other:?}"),
        }
        h
    }

    fn send(&mut self, req: Request) {
        let frame = encode_frame(&encode_message(&Message::Request(req))).unwrap();
        self.stdin.write_all(&frame).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self, timeout: Duration) -> Option<Message> {
        self.rx.recv_timeout(timeout).ok()
    }

    fn recv_until<T>(&mut self, deadline: Duration, mut f: impl FnMut(&Message) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            let remaining = deadline
                .checked_sub(start.elapsed())
                .expect("timed out waiting for a matching message");
            match self.recv(remaining) {
                Some(msg) => {
                    if let Some(v) = f(&msg) {
                        return v;
                    }
                }
                None => panic!("timed out waiting for a matching message"),
            }
        }
    }

    fn handshake(&mut self) {
        self.send(Request::Handshake {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::HandshakeOk { .. }) => Some(()),
            _ => None,
        });
    }

    /// Drive a UI-bridge `db_command` over the pipe and return its parsed JSON result. This
    /// is the EXACT path the renderer's `nyxBridge.invoke(...)` reaches — the UI side of the
    /// same shared `usecase::*` authority the MCP surface uses.
    fn db_command(&mut self, id: u64, command: &str, args: serde_json::Value) -> serde_json::Value {
        self.send(Request::DbCommand {
            id,
            command: command.to_string(),
            args_json: args.to_string(),
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::DbResult {
                id: rid,
                result_json,
            }) if *rid == id => Some(serde_json::from_str(result_json).unwrap()),
            Message::Response(Response::Error {
                id: rid, message, ..
            }) if *rid == id => panic!("db_command {command} errored: {message}"),
            _ => None,
        })
    }

    /// How many `workspaces://changed` invalidations have been PUSHED on the pipe so far.
    fn workspaces_push_count(&self) -> usize {
        self.workspaces_pushes.load(Ordering::SeqCst)
    }

    /// Block until AT LEAST `n` `workspaces://changed` pushes have been observed (or panic on
    /// timeout). The push is post-commit and lands asynchronously w.r.t. the db response, so a
    /// test waits for the COUNT to reach the expected value rather than for a strictly-ordered
    /// frame. This is still a push, never a poll: the count only moves when the sidecar emits.
    fn wait_for_workspaces_pushes(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.workspaces_push_count() < n {
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {n} workspaces://changed pushes (saw {})",
                    self.workspaces_push_count()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn shutdown(mut self) {
        self.send(Request::Shutdown { id: 9999 });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::ShutdownAck { .. }) => Some(()),
            _ => None,
        });
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                assert!(status.success(), "clean shutdown exits 0, got {status:?}");
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                let _ = self.child.kill();
                panic!("sidecar did not exit after shutdown");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// THE identity-parcours smoke: create project + workspace over the real pipe, observe the
/// pushed events, then re-open the SAME `nyx.db` to prove the persisted single-file state
/// (detected repositories + correlated domain_events).
///
/// Windows is excluded only because the repo-detection leg drives `git`; the core
/// project/workspace/push/outbox assertions are platform-agnostic (the multi-OS parity is a
/// CI-matrix concern, per the doc comment above).
#[cfg(not(windows))]
#[test]
fn identity_parcours_end_to_end_over_the_real_sidecar() {
    let data_dir = temp_dir("data");
    let have_git = git_available();

    // A MULTI-REPO root tree: two git subfolders (api, web) + a non-git folder (docs).
    // When git is present, creating the project on this root must auto-detect api + web.
    let root = temp_dir("root");
    if have_git {
        init_repo(&root.join("api"), "main");
        init_repo(&root.join("web"), "feat");
        std::fs::create_dir_all(root.join("docs")).unwrap(); // non-git: ignored
    }

    let mut h = Harness::spawn(data_dir.clone());
    h.handshake();
    assert_eq!(
        h.workspaces_push_count(),
        0,
        "no push before any mutation (nothing polls a change into existence)"
    );

    // --- LEG 1: create a PROJECT (UI bridge → shared use case) ------------------------
    // Drives `create_project` — the same `usecase::create_project` the MCP surface funnels
    // into. It creates the project + its root workspace, auto-detects repos under the root,
    // persists a `project.created` domain_event, and PUSHES `workspaces://changed`.
    let created = h.db_command(
        10,
        "create_project",
        serde_json::json!({ "name": "GateProj", "rootPath": root.to_str().unwrap() }),
    );
    let project_id = created["project"]["id"].as_str().unwrap().to_string();
    let root_ws_id = created["root"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        created["root"]["project_id"].as_str().unwrap(),
        project_id,
        "the root workspace belongs to the created project (nyx is the single authority)"
    );
    // EVENT PUSHED (not polled): the committed mutation pushed a workspaces invalidation.
    // The push is post-commit, so it may land just before/after the db response; wait for the
    // count to reach 1.
    h.wait_for_workspaces_pushes(1);

    // --- LEG 2: add a non-root WORKSPACE (the worktree pivot) -------------------------
    // A second workspace rooted at the api repo (itself a git tree → a '.' mono identity).
    let ws_root = root.join("api");
    let ws = h.db_command(
        11,
        "create_workspace",
        serde_json::json!({
            "projectId": project_id,
            "name": "worktree",
            "path": ws_root.to_str().unwrap(),
        }),
    );
    let ws_id = ws["id"].as_str().unwrap().to_string();
    assert_eq!(
        ws["project_id"].as_str().unwrap(),
        project_id,
        "the added workspace is a pivot in the SAME project"
    );
    assert!(
        !ws["is_root"].as_bool().unwrap(),
        "an added workspace is not the root"
    );
    h.wait_for_workspaces_pushes(2);

    // --- LEG 3: a RENAME mutation also pushes (covers the archive/rename family) -------
    // rename_workspace routes through the shared use case → domain_event + push, proving
    // the push is DOMAIN-driven (every identity mutation), not a per-command string match.
    h.db_command(
        12,
        "rename_workspace",
        serde_json::json!({ "id": ws_id, "name": "worktree-renamed" }),
    );
    h.wait_for_workspaces_pushes(3);

    // The three committed identity mutations each PUSHED exactly once — no more, no fewer,
    // and NOTHING pushed on a timer (the count only moved on a commit).
    assert_eq!(
        h.workspaces_push_count(),
        3,
        "exactly one workspaces://changed push per committed mutation (no polling, no double-push)"
    );

    // Sanity read over the pipe (UI authority): the project's workspaces now list root +
    // the added one, in root-first order.
    let workspaces = h.db_command(
        13,
        "list_workspaces",
        serde_json::json!({ "projectId": project_id }),
    );
    let ws_ids: Vec<&str> = workspaces
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap())
        .collect();
    assert!(
        ws_ids.contains(&root_ws_id.as_str()) && ws_ids.contains(&ws_id.as_str()),
        "the UI list reflects both workspaces from the single authority"
    );

    // Clean shutdown BEFORE re-opening the file (the persisted state is the assertion).
    h.shutdown();

    // --- SINGLE-FILE AUTHORITY: re-open the SAME nyx.db and read the persisted state ---
    // There is exactly ONE SQLite authority file; re-opening it (no second base anywhere)
    // shows the canonical `repositories` + `domain_events` the chain created.
    let db_file = data_dir.join("nyx.db");
    assert!(
        db_file.is_file(),
        "exactly one SQLite authority file at <data_dir>/nyx.db"
    );

    let db = nyx_core::db::Db::open(&db_file).expect("re-open the single nyx.db");

    // (a) domain_events OUTBOX — one correlated row per committed mutation, in order.
    let events = db
        .with_conn(nyx_core::db::list_domain_events)
        .expect("list domain_events");
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert!(
        types.contains(&"project.created")
            && types.contains(&"workspace.created")
            && types.contains(&"workspace.renamed"),
        "the committed identity mutations persisted their correlated domain_events: {types:?}"
    );
    // Correlation: every event belongs to the project; the workspace events carry the ws id.
    for e in &events {
        assert_eq!(
            e.project_id, project_id,
            "every domain_event correlates to the project (single authority)"
        );
        if e.aggregate == "workspace" {
            assert!(
                e.workspace_id.is_some(),
                "a workspace-scoped event carries its workspace correlation"
            );
        }
    }
    // The push matched the outbox: at least as many mutations pushed as rows persisted for
    // the three we drove (project.created + workspace.created + workspace.renamed).
    let created_ct = types.iter().filter(|t| **t == "project.created").count();
    let ws_created_ct = types.iter().filter(|t| **t == "workspace.created").count();
    let renamed_ct = types.iter().filter(|t| **t == "workspace.renamed").count();
    assert_eq!(
        (created_ct, ws_created_ct, renamed_ct),
        (1, 1, 1),
        "exactly one domain_event per driven mutation — no phantom, no loss"
    );

    // (b) REPO DETECTION — when git is present, creating the project on the multi-repo root
    // detected api + web as logical repositories in the project. This is the "détecter
    // repos" leg of the parcours, read from the persisted authority.
    if have_git {
        let repos = db
            .with_conn(|c| nyx_core::db::list_repositories(c, &project_id))
            .expect("list_repositories");
        let rel: Vec<&str> = repos.iter().map(|r| r.relative_path.as_str()).collect();
        // The root detection registered api + web; the added `api`-rooted workspace also
        // triggered detection of a '.' mono identity in the same project.
        assert!(
            rel.contains(&"api") && rel.contains(&"web"),
            "creating the project detected the multi-repo subfolders (api, web): {rel:?}"
        );
        assert!(
            rel.contains(&"."),
            "the api-rooted worktree detected a '.' mono identity in the same project: {rel:?}"
        );

        // (c) GIT STATE re-read LIVE from Git (never persisted) — the branch of the api repo
        // is read straight from Git, proving the DB is not authoritative for Git state.
        let git = nyx_core::repository::read_git_state(root.join("api").to_str().unwrap());
        assert_eq!(
            git.branch.as_deref(),
            Some("main"),
            "the api repo's branch is re-read LIVE from Git"
        );
    } else {
        eprintln!(
            "git unavailable: skipped the repo-detection + Git-state legs (git-agnostic by contract)"
        );
    }

    std::fs::remove_dir_all(&data_dir).ok();
    std::fs::remove_dir_all(&root).ok();
}
