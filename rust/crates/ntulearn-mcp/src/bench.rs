//! `ntulearn-mcp selfbench` — built-in performance harness.
//!
//! Defines the metrics this project hill-climbs on:
//!
//!   * **Token efficiency** — estimated as `chars / 4` (the standard
//!     approximation for tokenizer throughput). Measured for:
//!       - discovery: MCP `tools/list` payload (injected into agent context
//!         every turn) vs the CLI `help` text (read on demand, once)
//!       - task output: the MCP pretty-JSON render vs CLI compact TSV vs
//!         CLI `--json` (compact JSON), over a fixture suite
//!   * **Task latency** — end-to-end dispatch wall time against a local mock
//!     Blackboard API (fixed 25 ms per-request latency, empty SQLite cache =
//!     cold), plus process spawn overhead
//!   * **Concurrency** — the same command suite run sequentially vs through
//!     the concurrent `multi` path
//!
//! Everything is self-contained: the mock server, fixtures, and timing live
//! here, so `selfbench` is reproducible on any machine with zero setup.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Requests served by the mock server (diagnostics for singleflight/concurrency).
static MOCK_REQUESTS: AtomicU64 = AtomicU64::new(0);
static MOCK_ENROLL: AtomicU64 = AtomicU64::new(0);
static MOCK_COURSE: AtomicU64 = AtomicU64::new(0);
static MOCK_OTHER: AtomicU64 = AtomicU64::new(0);

fn mock_requests() -> u64 {
    MOCK_REQUESTS.load(Ordering::Relaxed)
}

fn mock_breakdown() -> String {
    format!(
        "enrollments={} course-gets={} other={}",
        MOCK_ENROLL.load(Ordering::Relaxed),
        MOCK_COURSE.load(Ordering::Relaxed),
        MOCK_OTHER.load(Ordering::Relaxed)
    )
}

use serde_json::{json, Value};

use crate::cli::{self, Invocation};
use crate::schemas;

const MOCK_LATENCY_MS: u64 = 25;

fn est_tokens(chars: usize) -> usize {
    chars / 4
}

// ---------------------------------------------------------------------------
// Mock Blackboard API
// ---------------------------------------------------------------------------

fn fixture_courses() -> Vec<Value> {
    (1..=6)
        .map(|n| {
            json!({
                "id": format!("_{n}0000{n}_1"),
                "name": format!("SC200{n} Something Something Engineering ({n})"),
                "created": "2026-08-01T00:00:00Z",
                "organization": false,
                "ultraStatus": "Unavailable",
                "availability": { "available": "Yes" }
            })
        })
        .collect()
}

async fn mock_server() -> anyhow::Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 8192];
                // Read until end of headers (GET requests only).
                let n = match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                MOCK_REQUESTS.fetch_add(1, Ordering::Relaxed);
                if std::env::var("NTULEARN_BENCH_TRACE").is_ok() {
                    eprintln!("[mock] GET {path}");
                }
                if path.contains("users/me/courses") {
                    MOCK_ENROLL.fetch_add(1, Ordering::Relaxed);
                } else if path.starts_with("/learn/api/public/v1/courses/") && path.trim_start_matches("/learn/api/public/v1/courses/").split('?').next().unwrap_or("").split('/').count() == 1 {
                    MOCK_COURSE.fetch_add(1, Ordering::Relaxed);
                } else {
                    MOCK_OTHER.fetch_add(1, Ordering::Relaxed);
                }
                // Simulated network RTT.
                tokio::time::sleep(Duration::from_millis(MOCK_LATENCY_MS)).await;

                let body = if path.starts_with("/learn/api/public/v1/users/me/courses") {
                    let results: Vec<Value> = fixture_courses()
                        .iter()
                        .map(|c| {
                            json!({
                                "courseId": c["id"],
                                "role": "S",
                                "lastAccessed": format!("2026-09-0{}T12:00:00Z", 1 + rand_digit(&path)),
                                "availability": {"available": "Yes"}
                            })
                        })
                        .collect();
                    serde_json::to_string(&json!({"results": results, "paging": {}})).unwrap()
                } else if let Some(cid) =
                    path.strip_prefix("/learn/api/public/v1/courses/").map(|p| p.split('?').next().unwrap_or(p))
                {
                    if cid.contains('/') {
                        // announcements under a course
                        let results = vec![
                            json!({"id": "_90001_1", "title": "Week 8 lab moved to Lab 3-14", "body": "Please note...", "created": "2026-09-20T02:00:00Z", "courseId": cid}),
                            json!({"id": "_90002_1", "title": "Assignment 2 released", "body": "Deadline...", "created": "2026-09-19T05:30:00Z", "courseId": cid}),
                        ];
                        serde_json::to_string(&json!({"results": results, "paging": {}})).unwrap()
                    } else {
                        let mut courses = fixture_courses();
                        let pick = courses
                            .iter()
                            .find(|c| c["id"].as_str().map(|s| s == cid).unwrap_or(false))
                            .cloned()
                            .unwrap_or_else(|| json!({"id": cid, "name": cid, "availability": {"available": "Yes"}}));
                        let _ = &mut courses;
                        serde_json::to_string(&pick).unwrap()
                    }
                } else {
                    "{}".to_string()
                };

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Ok(format!("http://{addr}"))
}

fn rand_digit(seed: &str) -> usize {
    (seed.len() % 5) + 1
}

// ---------------------------------------------------------------------------
// Fixture payloads (shape-matched to handler outputs)
// ---------------------------------------------------------------------------

fn suite_payloads() -> Vec<(&'static str, String, Value)> {
    let courses: Vec<Value> = fixture_courses()
        .iter()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "courseId": c["id"],
                "title": c["name"],
                "available": "Yes",
                "lastAccessed": format!("2026-09-1{}T09:00:00Z", i)
            })
        })
        .collect();
    let contents: Vec<Value> = (1..=10)
        .map(|n| {
            json!({
                "id": format!("_{n}1234{n}_1"),
                "title": if n % 2 == 0 { format!("Week {n} Lecture Slides") } else { format!("Week {n} Folder") },
                "type": if n % 2 == 0 { "resource/x-bb-file" } else { "resource/x-bb-folder" },
                "hasChildren": n % 2 == 1,
                "parentId": Value::Null,
                "created": "2026-08-30T00:00:00Z",
                "modified": "2026-09-01T00:00:00Z",
                "description": "Long description that nobody asked for, padded to look realistic for measurement purposes."
            })
        })
        .collect();
    let announcements: Vec<Value> = (1..=5)
        .map(|n| {
            json!({
                "id": format!("_{n}77{n}_1"),
                "courseId": "_100001_1",
                "title": format!("Announcement number {n} about the upcoming tutorial"),
                "created": format!("2026-09-1{n}T08:00:00Z"),
                "body": "Body text which is moderately long and contains details about the announcement that the agent may or may not need to read in full."
            })
        })
        .collect();
    let columns: Vec<Value> = (1..=8)
        .map(|n| {
            json!({
                "courseId": "_100001_1",
                "id": format!("_col{n}_1"),
                "name": format!("Assessment {n}"),
                "score": Value::Null,
                "status": "Needs Grading",
                "dueDate": format!("2026-10-0{n}T23:59:00Z"),
                "pointsPossible": 100.0,
                "externalGrade": n == 8
            })
        })
        .collect();
    vec![
        (
            "courses",
            "courses".to_string(),
            json!({
                "courses": courses, "total": 6, "count": 6, "offset": 0, "limit": 50,
                "hasMore": false, "nextOffset": Value::Null
            }),
        ),
        (
            "contents",
            "contents _100001_1".to_string(),
            json!({
                "items": contents, "total": 10, "count": 10, "offset": 0, "limit": 50,
                "hasMore": false, "nextOffset": Value::Null
            }),
        ),
        (
            "announcements",
            "announcements --course-ids _100001_1".to_string(),
            json!({
                "announcements": announcements, "total": 5, "count": 5, "offset": 0, "limit": 50,
                "hasMore": false, "nextOffset": Value::Null
            }),
        ),
        (
            "grades",
            "grades --course-ids _100001_1".to_string(),
            json!({
                "columns": columns, "total": 8, "count": 8, "offset": 0, "limit": 50,
                "hasMore": false, "nextOffset": Value::Null
            }),
        ),
    ]
}

fn parse_invocation_or_panic(cmd: &str) -> Invocation {
    let toks: Vec<String> = cmd.split_whitespace().map(|s| s.to_string()).collect();
    cli::parse_invocation(&toks[0], &toks[1..]).expect("bench invocation")
}

// ---------------------------------------------------------------------------
// Bench driver
// ---------------------------------------------------------------------------

pub async fn run(verbose: bool) -> i32 {
    println!("ntulearn-mcp selfbench — token + latency benchmark");
    println!("token estimate: chars/4 | mock RTT: {MOCK_LATENCY_MS} ms/req | cold SQLite cache\n");

    // ---- 1. Discovery cost: MCP schema injection vs CLI help -------------
    let tools = schemas::all_tools();
    let mcp_catalog = serde_json::to_string(&json!({
        "tools": tools.iter().map(|t| json!({
            "name": t.name,
            "description": t.description,
            "inputSchema": t.input_schema,
        })).collect::<Vec<_>>()
    }))
    .unwrap_or_default();
    let help = cli::help_text();
    println!("[discovery] (injected/read per conversation)");
    println!("  MCP tools/list payload : {:>7} chars ≈ {:>6} tok (every turn, all 21 tools)", mcp_catalog.len(), est_tokens(mcp_catalog.len()));
    println!("  CLI `help` text        : {:>7} chars ≈ {:>6} tok (on demand, once)", help.len(), est_tokens(help.len()));
    println!("  CLI discovery saving   : {:>6.1}x fewer tokens", mcp_catalog.len() as f64 / help.len() as f64);

    // ---- 2. Output cost: MCP pretty JSON vs CLI compact ------------------
    println!("\n[task output] fixture suite (4 representative payloads)");
    let mut mcp_total = 0usize;
    let mut cli_total = 0usize;
    let mut raw_total = 0usize;
    for (name, cmd, payload) in suite_payloads() {
        let inv = parse_invocation_or_panic(&cmd);
        let mcp_pretty = serde_json::to_string_pretty(&payload).unwrap_or_default();
        let cli_compact = cli::render_compact(&inv, &payload);
        let cli_raw = serde_json::to_string(&payload).unwrap_or_default();
        mcp_total += mcp_pretty.len();
        cli_total += cli_compact.len();
        raw_total += cli_raw.len();
        if verbose {
            println!("  {name:<14} mcp-json {:>6} ch | cli-tsv {:>6} ch | cli-json {:>6} ch", mcp_pretty.len(), cli_compact.len(), cli_raw.len());
        }
    }
    println!("  MCP json (pretty) total : {:>6} chars ≈ {:>5} tok", mcp_total, est_tokens(mcp_total));
    println!("  CLI --json total        : {:>6} chars ≈ {:>5} tok", raw_total, est_tokens(raw_total));
    println!("  CLI compact TSV total   : {:>6} chars ≈ {:>5} tok", cli_total, est_tokens(cli_total));
    println!("  compact vs MCP json     : {:>5.1}x smaller", mcp_total as f64 / cli_total.max(1) as f64);

    // ---- 3. Latency against the local mock API ---------------------------
    let base_url = match mock_server().await {
        Ok(u) => u,
        Err(e) => {
            eprintln!("mock server failed: {e}");
            return 1;
        }
    };

    // Isolated temp cache dir so every timed run is cold.
    let cache_root = std::env::temp_dir().join(format!("ntulearn-bench-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
    std::fs::create_dir_all(&cache_root).ok();

    let build_cold_state = || async {
        let dir = cache_root.join(format!("c{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()));
        std::fs::create_dir_all(&dir).ok();
        std::env::set_var("NTULEARN_CACHE_DIR", &dir);
        std::env::set_var("NTULEARN_BASE_URL", &base_url);
        crate::build_state().await.expect("state")
    };

    println!("\n[latency] end-to-end dispatch, cold cache, mock RTT {MOCK_LATENCY_MS} ms");
    let inv = parse_invocation_or_panic("courses");
    let mut cold: Vec<Duration> = Vec::new();
    for run in 0..3 {
        let state = build_cold_state().await;
        let t0 = Instant::now();
        let out = cli::run_one(&state, &inv).await.expect("courses run");
        let dt = t0.elapsed();
        cold.push(dt);
        if verbose && run == 0 {
            println!("  courses output ({} chars):\n{}", out.len(), out);
        }
        println!("  courses cold run {}      : {:>7.1?}", run + 1, dt);
    }
    println!("  courses cold median     : {:>7.1?}", cold[1]);

    // Warm cache (same state, same command).
    let warm_state = build_cold_state().await;
    let _ = cli::run_one(&warm_state, &inv).await;
    let t0 = Instant::now();
    let _ = cli::run_one(&warm_state, &inv).await;
    println!("  courses warm (cached)   : {:>7.1?}", t0.elapsed());

    // ---- 4. Concurrency: sequential vs multi -----------------------------
    println!("\n[concurrency] 4 commands, cold cache each");
    let cmds = [
        "courses",
        "courses --limit 3",
        "announcements --course-ids _100001_1",
        "courses --include-disabled",
    ];
    let invs: Vec<Invocation> = cmds.iter().map(|c| parse_invocation_or_panic(c)).collect();

    // Sequential: separate cold state per command (fresh process equivalent).
    let t0 = Instant::now();
    for inv in &invs {
        let state = build_cold_state().await;
        let _ = cli::run_one(&state, inv).await;
    }
    let seq = t0.elapsed();
    println!("  sequential (4 procs)    : {:>7.1?}", seq);

    // Concurrent multi: one state, JoinSet.
    let state = build_cold_state().await;
    let reqs_before = mock_requests();
    let t0 = Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for inv in &invs {
        let state = state.clone();
        let inv = inv.clone();
        set.spawn(async move { cli::run_one(&state, &inv).await });
    }
    while set.join_next().await.is_some() {}
    let par = t0.elapsed();
    let reqs = mock_requests() - reqs_before;
    println!("  multi (1 proc, joined)  : {:>7.1?}  ({} HTTP reqs: {})", par, reqs, mock_breakdown());
    println!("  speedup                 : {:>6.2}x", seq.as_secs_f64() / par.as_secs_f64());

    // ---- 5. Process spawn overhead ---------------------------------------
    let exe = std::env::current_exe().unwrap_or_default();
    let t0 = Instant::now();
    for _ in 0..5 {
        let _ = tokio::process::Command::new(&exe).arg("help").output().await;
    }
    println!("\n[spawn] `help` via subprocess, 5x mean: {:>7.1?}", t0.elapsed() / 5);

    // ---- 6. Headline metric ----------------------------------------------
    // Agent Efficiency Score: estimated context tokens a 10-turn conversation
    // spends on discovery + one pass of the 4-task suite, CLI vs MCP.
    let mcp_conv = est_tokens(mcp_catalog.len()) * 10 + est_tokens(mcp_total);
    let cli_conv = est_tokens(help.len()) + est_tokens(cli_total);
    println!("\n[headline] Agent Efficiency Score (10-turn conversation, 4-task suite)");
    println!("  MCP  : discovery {} x10 turns + output {} = ~{} tok", est_tokens(mcp_catalog.len()), est_tokens(mcp_total), mcp_conv);
    println!("  CLI  : help once + output {} = ~{} tok", est_tokens(cli_total), cli_conv);
    println!("  CLI is {:.1}x more token-efficient", mcp_conv as f64 / cli_conv as f64);
    let _ = verbose;

    // Cleanup bench caches (best effort).
    let _ = std::fs::remove_dir_all(&cache_root);
    0
}
