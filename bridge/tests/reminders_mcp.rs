//! `jesse-reminders-mcp` over stdio, the binary itself: `tools/list` is exactly the four tools,
//! `create_reminder` with a bare `06:45` resolves to the next 06:45 in the PROFILE zone (an away
//! profile in `profile.json`, read as the bridge reads it), and every invalid input comes back as
//! a tool error naming what was wrong.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_jesse-reminders-mcp");

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "jesse-reminders-mcp-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An away profile in `zone`, in force for decades: the zone the server must read.
fn write_profile(dir: &Path, zone: &str) {
    let now = Utc::now().timestamp_millis();
    std::fs::write(
        dir.join("profile.json"),
        json!({
            "profile": {
                "name": "away",
                "tz": zone,
                "since_ms": now - 1000,
                "until_ms": now + 20 * 365 * 86_400_000_i64,
                "note": "test"
            }
        })
        .to_string(),
    )
    .unwrap();
}

/// Run one session: each request in order, every response back.
fn session(dir: &Path, requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(BIN)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .env("JESSE_STATE_DIR", dir)
        // The host zone is UTC: a server that read it instead of the profile would be an hour
        // or more out, and the assertions below would see it.
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn jesse-reminders-mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut replies = Vec::new();
    for req in requests {
        writeln!(stdin, "{req}").unwrap();
        stdin.flush().unwrap();
        if req.get("id").is_none() {
            continue;
        }
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        replies.push(serde_json::from_str(&line).unwrap());
    }
    drop(stdin);
    child.wait().unwrap();
    replies
}

fn call(id: u64, name: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}})
}

fn init() -> Vec<Value> {
    vec![
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ]
}

fn text_of(reply: &Value) -> (bool, String) {
    let r = &reply["result"];
    (
        r["isError"].as_bool().unwrap(),
        r["content"][0]["text"].as_str().unwrap().to_string(),
    )
}

#[test]
fn tools_list_is_exactly_the_four_tools() {
    let dir = scratch();
    let mut reqs = init();
    reqs.push(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
    let replies = session(&dir, &reqs);
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(
        replies[0]["result"]["serverInfo"]["name"],
        "jesse-reminders-mcp"
    );
    let names: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "create_reminder",
            "list_reminders",
            "cancel_reminder",
            "snooze_reminder"
        ]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn create_at_06_45_resolves_to_the_next_06_45_in_the_profile_zone() {
    let dir = scratch();
    write_profile(&dir, "Asia/Tokyo");
    let before = Utc::now();
    let mut reqs = init();
    reqs.push(call(
        1,
        "create_reminder",
        json!({"text": "start the pancakes", "at": "06:45"}),
    ));
    reqs.push(call(2, "list_reminders", json!({})));
    let replies = session(&dir, &reqs);
    let (is_err, text) = text_of(&replies[1]);
    println!("create_reminder(06:45) replied:\n{text}");
    assert!(!is_err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["zone"], "Asia/Tokyo");
    assert_eq!(v["zone_mode"], "profile");
    assert_eq!(v["state"], "active");
    let local = v["next_fire_local"].as_str().unwrap();
    assert!(local.ends_with("T06:45+09:00"), "{local}");
    let utc: DateTime<Utc> = v["next_fire_utc"].as_str().unwrap().parse().unwrap();
    // 06:45 in Tokyo is 21:45 UTC, and it is the NEXT one: within a day of now.
    assert_eq!(utc.format("%H:%M").to_string(), "21:45");
    assert!(utc > before && utc <= before + Duration::hours(24), "{utc}");
    assert!(v["confirmation"]
        .as_str()
        .unwrap()
        .contains("06:45 Asia/Tokyo"));

    // It is on disk, in the one file, and listed.
    let (_, listed) = text_of(&replies[2]);
    let listed: Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(listed["reminders"][0]["id"], v["id"]);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let mut names: Vec<&str> = names.iter().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["profile.json", "reminders.json", "reminders.json.lock"]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn every_invalid_input_returns_its_error() {
    let dir = scratch();
    write_profile(&dir, "Europe/Rome");
    let past = (Utc::now() - Duration::days(2))
        .format("%Y-%m-%dT%H:%M")
        .to_string();
    let soon = (Utc::now() + Duration::days(10))
        .format("%Y-%m-%dT07:00")
        .to_string();
    let early = (Utc::now() + Duration::days(3))
        .format("%Y-%m-%d")
        .to_string();
    let cases = [
        (json!({"text": "   ", "at": "07:00"}), "`text` is empty"),
        (json!({"text": "x", "at": past}), "in the past"),
        (
            json!({"text": "x", "at": soon, "recurrence": {"kind": "daily", "until": early}}),
            "falls before the first fire",
        ),
        (json!({"text": "x", "at": "half seven"}), "`at` must be"),
        (json!({"text": "x"}), "`at` is required"),
        (
            json!({"text": "x", "at": "07:00", "zone": "Nowhere/Place"}),
            "IANA",
        ),
        (
            json!({"text": "x", "at": "07:00", "recurrence": {"kind": "weekly", "days": ["funday"]}}),
            "not a weekday",
        ),
    ];
    let mut reqs = init();
    for (i, (args, _)) in cases.iter().enumerate() {
        reqs.push(call(i as u64 + 1, "create_reminder", args.clone()));
    }
    let n = cases.len() as u64;
    reqs.push(call(n + 1, "cancel_reminder", json!({"id": "rnope00"})));
    reqs.push(call(
        n + 2,
        "snooze_reminder",
        json!({"id": "rnope00", "minutes": 10}),
    ));
    reqs.push(call(n + 3, "snooze_reminder", json!({"id": "rnope00"})));
    reqs.push(call(n + 4, "no_such_tool", json!({})));
    let replies = session(&dir, &reqs);
    for (i, (_, want)) in cases.iter().enumerate() {
        let (is_err, text) = text_of(&replies[i + 1]);
        assert!(is_err, "case {i}: {text}");
        assert!(text.contains(want), "case {i}: {text:?} lacks {want:?}");
    }
    let tail = &replies[cases.len() + 1..];
    assert!(text_of(&tail[0]).1.contains("no reminder with id"));
    assert!(text_of(&tail[1]).1.contains("no reminder with id"));
    assert!(text_of(&tail[2]).1.contains("`minutes` is required"));
    assert_eq!(tail[3]["error"]["code"], -32602);
    // Nothing invalid reached the store.
    let stored: Value =
        serde_json::from_slice(&std::fs::read(dir.join("reminders.json")).unwrap_or_default())
            .unwrap_or(json!({"reminders": []}));
    assert_eq!(stored["reminders"].as_array().map(Vec::len).unwrap_or(0), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cancel_and_snooze_act_on_a_created_reminder() {
    let dir = scratch();
    write_profile(&dir, "Europe/Rome");
    let mut reqs = init();
    reqs.push(call(
        1,
        "create_reminder",
        json!({"text": "take the bins out", "at": "07:30", "recurrence": "weekdays"}),
    ));
    let replies = session(&dir, &reqs);
    let created: Value = serde_json::from_str(&text_of(&replies[1]).1).unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["recurrence"], json!({"kind": "weekdays"}));

    let mut reqs = init();
    reqs.push(call(1, "snooze_reminder", json!({"id": id, "minutes": 15})));
    reqs.push(call(2, "cancel_reminder", json!({"id": id})));
    reqs.push(call(3, "list_reminders", json!({})));
    reqs.push(call(4, "list_reminders", json!({"include_inactive": true})));
    let replies = session(&dir, &reqs);
    let snoozed: Value = serde_json::from_str(&text_of(&replies[1]).1).unwrap();
    assert!(snoozed["snoozed_until_utc"].is_string());
    let cancelled: Value = serde_json::from_str(&text_of(&replies[2]).1).unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    let active: Value = serde_json::from_str(&text_of(&replies[3]).1).unwrap();
    assert_eq!(active["reminders"], json!([]));
    let all: Value = serde_json::from_str(&text_of(&replies[4]).1).unwrap();
    assert_eq!(all["reminders"][0]["state"], "cancelled");
    let _ = std::fs::remove_dir_all(dir);
}
