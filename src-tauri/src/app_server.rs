use crate::domain::QuotaSample;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn read_codex_quota(timeout: Duration) -> Result<Vec<QuotaSample>> {
    read_codex_quota_with_command(codex_app_server_command(), timeout)
}

fn read_codex_quota_with_command(
    mut command: Command,
    timeout: Duration,
) -> Result<Vec<QuotaSample>> {
    if std::env::var_os("METRIK_DEBUG").is_some() {
        eprintln!("app-server command: {command:?}");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }

    let mut child = ManagedChild::new(
        command
            .spawn()
            .context("failed to start codex app-server")?,
    );
    let mut stdin = child
        .child_mut()
        .stdin
        .take()
        .context("codex app-server stdin unavailable")?;
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .context("codex app-server stdout unavailable")?;
    let (sender, receiver) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    write_json(
        &mut stdin,
        &json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": { "name": "metrik", "title": "Metrik", "version": "0.1.0" },
                "capabilities": { "experimentalApi": true, "optOutNotificationMethods": [] }
            }
        }),
    )?;

    let deadline = Instant::now() + timeout;
    let mut sent_request = false;
    let mut result = None;

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = receiver.recv_timeout(remaining.min(Duration::from_millis(250))) else {
            continue;
        };
        if std::env::var_os("METRIK_DEBUG").is_some() {
            eprintln!("app-server << {line}");
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = value.get("id").and_then(Value::as_i64);
        if id == Some(1) && !sent_request {
            write_json(&mut stdin, &json!({ "method": "initialized" }))?;
            write_json(
                &mut stdin,
                &json!({ "id": 3, "method": "account/rateLimits/read" }),
            )?;
            sent_request = true;
        } else if id == Some(3) {
            result = value.get("result").cloned();
            break;
        }
    }

    drop(stdin);
    child.terminate();

    let result = result.context("codex app-server quota request timed out")?;
    Ok(parse_rate_limits(&result))
}

struct ManagedChild {
    child: Option<Child>,
}

impl ManagedChild {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("managed child already terminated")
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_process_tree(&mut child);
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn terminate_process_tree(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }

    #[cfg(windows)]
    terminate_windows_process_tree(child.id());

    // This is the cross-platform fallback and also reaps the direct child after
    // Windows has terminated its descendants.
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn terminate_windows_process_tree(pid: u32) {
    use std::os::windows::process::CommandExt;

    let mut taskkill = Command::new("taskkill.exe");
    taskkill
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000);
    let _ = taskkill.status();
}

fn codex_app_server_command() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let explicit = std::env::var_os("CODEX_BINARY").map(PathBuf::from);
        let npm_script = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("npm").join("codex.cmd"))
            .filter(|path| path.exists());
        let script = explicit
            .or(npm_script)
            .unwrap_or_else(|| PathBuf::from("codex"));
        let mut command = Command::new("cmd.exe");
        command
            .args(["/D", "/C"])
            .arg(script)
            .args(["app-server", "--stdio"])
            .creation_flags(0x0800_0000);
        command
    }

    #[cfg(not(windows))]
    {
        let mut command = Command::new(resolve_unix_codex_binary());
        command.args(["app-server", "--stdio"]);
        command
    }
}

#[cfg(not(windows))]
fn resolve_unix_codex_binary() -> PathBuf {
    if let Some(explicit) = std::env::var_os("CODEX_BINARY") {
        return PathBuf::from(explicit);
    }

    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.extend([
            home.join(".local/bin/codex"),
            home.join(".npm-global/bin/codex"),
            home.join(".volta/bin/codex"),
            home.join(".bun/bin/codex"),
            home.join(".local/share/pnpm/codex"),
            home.join("Library/pnpm/codex"),
        ]);

        let nvm_root = home.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(nvm_root) {
            let mut nvm_candidates: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path().join("bin/codex"))
                .filter(|path| path.is_file())
                .collect();
            nvm_candidates.sort();
            nvm_candidates.reverse();
            candidates.extend(nvm_candidates);
        }
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
        PathBuf::from("/usr/bin/codex"),
    ]);

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("codex"))
}

fn write_json(stdin: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *stdin, value)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn parse_rate_limits(result: &Value) -> Vec<QuotaSample> {
    let limits = result
        .get("rateLimitsByLimitId")
        .and_then(|value| value.get("codex"))
        .or_else(|| result.get("rateLimits"));
    let Some(limits) = limits else {
        return Vec::new();
    };
    let now = chrono::Utc::now().timestamp_millis();
    ["primary", "secondary"]
        .into_iter()
        .filter_map(|window_key| {
            let window = limits.get(window_key)?;
            let used = number(window.get("usedPercent")?)?;
            let resets_at_ms = window
                .get("resetsAt")
                .and_then(integer)
                .map(|value| value * 1000);
            Some(QuotaSample {
                adapter_id: "codex",
                window_key,
                remaining_percent: (100.0 - used).clamp(0.0, 100.0),
                resets_at_ms,
                collected_at_ms: now,
                source_label: "Codex app-server".into(),
                quality: "official_live",
            })
        })
        .collect()
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|value| value as f64))
}

fn integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().map(|value| value as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_primary_and_secondary_windows() {
        let value = json!({
            "rateLimits": {
                "primary": { "usedPercent": 26, "resetsAt": 1783831562 },
                "secondary": { "usedPercent": 9, "resetsAt": 1784371617 }
            }
        });
        let samples = parse_rate_limits(&value);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].remaining_percent, 74.0);
        assert_eq!(samples[1].remaining_percent, 91.0);
    }

    #[cfg(windows)]
    #[test]
    fn timeout_terminates_windows_process_tree() {
        use std::os::windows::process::CommandExt;

        let marker = std::env::temp_dir().join(format!(
            "metrik-process-tree-{}-{}.txt",
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        ));
        let escaped_marker = marker.to_string_lossy().replace("'", "''");
        let script = format!(
            "$child = Start-Process -FilePath \"$env:SystemRoot\\System32\\PING.EXE\" \
             -ArgumentList '-n','60','127.0.0.1' -WindowStyle Hidden -PassThru; \
             $child.Id | Set-Content -LiteralPath '{escaped_marker}' -Encoding ascii; \
             Wait-Process -Id $child.Id"
        );
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ]);

        let result = read_codex_quota_with_command(command, Duration::from_secs(2));
        assert!(result.is_err());

        let descendant_pid = std::fs::read_to_string(&marker)
            .expect("test child should record its descendant pid")
            .trim()
            .parse::<u32>()
            .expect("recorded descendant pid should be numeric");
        let filter = format!("PID eq {descendant_pid}");
        let output = Command::new("tasklist.exe")
            .args(["/FI", &filter, "/FO", "CSV", "/NH"])
            .creation_flags(0x0800_0000)
            .output()
            .expect("tasklist should inspect the descendant");
        let listing = String::from_utf8_lossy(&output.stdout);
        assert!(
            !listing.contains(&format!("\"{descendant_pid}\"")),
            "timed-out app-server descendant {descendant_pid} is still running"
        );
        let _ = std::fs::remove_file(marker);
    }

    #[test]
    #[ignore = "starts the current user's Codex app-server"]
    fn live_app_server_smoke_test() {
        let samples = read_codex_quota(Duration::from_secs(8)).unwrap();
        println!("live quota samples: {samples:?}");
        assert!(!samples.is_empty());
    }
}
