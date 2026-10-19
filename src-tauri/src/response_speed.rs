//! Opt-in loopback OTLP ingress. Raw exports live only in a bounded request
//! buffer; only validated speed metadata reaches the independent sample table.
use crate::{speed_config::ConfigManager, speed_store, speed_telemetry::Decoder, storage};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use flate2::read::GzDecoder;
use serde::Serialize;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const PORT: u16 = 46319;
const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const SOURCES: [&str; 2] = ["claude", "codex"];

fn setting(agent: &str) -> String {
    format!("response_speed.{agent}.enabled")
}
fn endpoint(agent: &str) -> String {
    let signal = if agent == "claude" { "logs" } else { "traces" };
    format!("http://127.0.0.1:{PORT}/{agent}/v1/{signal}")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionStatus {
    pub running: bool,
    pub sources: Vec<SourceStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    id: String,
    enabled: bool,
    configured: bool,
    conflict: bool,
    sample_count: i64,
    last_sample_at_ms: Option<i64>,
    error: Option<String>,
}

struct Worker {
    stop: Arc<AtomicBool>,
    address: SocketAddr,
    thread: JoinHandle<()>,
}
impl Worker {
    fn stop(self) {
        self.stop.store(true, Ordering::Release);
        // Wake the blocking listener. A current export is bounded by time/size.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
        let _ = self.thread.join();
    }
}

pub struct Collector {
    database: PathBuf,
    config: Arc<ConfigManager>,
    worker: Mutex<Option<Worker>>,
}

impl Collector {
    pub fn new(database: PathBuf) -> Self {
        let state_dir = database
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        Self {
            database,
            config: Arc::new(ConfigManager::detected(state_dir)),
            worker: Mutex::new(None),
        }
    }

    pub fn restore(&self) -> Result<()> {
        let connection = storage::open_database(&self.database)?;
        for agent in SOURCES {
            if is_enabled(&connection, agent)?
                && self
                    .config
                    .inspect(agent, &endpoint(agent))
                    .is_ok_and(|status| status.configured && !status.conflict)
            {
                self.start()?;
                break;
            }
        }
        Ok(())
    }

    pub fn status(&self) -> Result<CollectionStatus> {
        let connection = storage::open_database_read_only(&self.database)?;
        let running = self
            .worker
            .lock()
            .map_err(|_| anyhow::anyhow!("speed collector unavailable"))?
            .as_ref()
            .is_some_and(|worker| !worker.thread.is_finished());
        let now = Utc::now().timestamp_millis();
        let mut sources = Vec::new();
        for agent in SOURCES {
            let enabled = is_enabled(&connection, agent)?;
            let (configured, conflict, mut error) =
                match self.config.inspect(agent, &endpoint(agent)) {
                    Ok(status) => (status.configured, status.conflict, None),
                    Err(_) => (
                        false,
                        true,
                        Some(
                            crate::i18n::tr!(
                                "客户端计时配置读取失败。",
                                "Could not read the client timing configuration."
                            )
                            .to_owned(),
                        ),
                    ),
                };
            if enabled && configured && !conflict && !running {
                error = Some(
                    crate::i18n::tr!(
                        "本机速度采集服务未运行，请关闭后重新开启。",
                        "The local speed collector is stopped. Turn collection off and on again."
                    )
                    .to_owned(),
                );
            }
            let (sample_count, last_sample_at_ms) =
                speed_store::source_counts(&connection, agent, now)?;
            sources.push(SourceStatus {
                id: agent.into(),
                enabled,
                configured,
                conflict,
                sample_count,
                last_sample_at_ms,
                error,
            });
        }
        Ok(CollectionStatus { running, sources })
    }

    pub fn set_enabled(&self, agent: &str, enabled: bool) -> Result<CollectionStatus> {
        if !SOURCES.contains(&agent) {
            bail!("unsupported speed collection source");
        }
        let connection = storage::open_database(&self.database)
            .map_err(|_| anyhow::anyhow!("Could not open the local speed collection settings"))?;
        let was_enabled = is_enabled(&connection, agent)
            .map_err(|_| anyhow::anyhow!("Could not read the local speed collection settings"))?;
        if enabled {
            // Bind before changing a client. An occupied port must not leave a
            // client pointing at a service we failed to start.
            self.start()?;
            if let Err(error) = self.config.enable(agent, &endpoint(agent)) {
                self.stop_if_unused(&connection)?;
                return Err(error);
            }
            if storage::set_app_setting(&connection, &setting(agent), "1").is_err() {
                if !was_enabled {
                    let _ = self.config.disable(agent);
                }
                self.stop_if_unused(&connection)?;
                bail!("could not save the speed collection setting");
            }
        } else {
            // Validate the settings write before editing the client, and keep
            // the two changes together for normal database/config failures.
            let transaction = connection
                .unchecked_transaction()
                .map_err(|_| anyhow::anyhow!("Could not update the speed collection setting"))?;
            storage::set_app_setting(&transaction, &setting(agent), "0").map_err(|_| {
                anyhow::anyhow!("Could not save the speed collection setting; retry turning it off")
            })?;
            self.config.disable(agent)?;
            transaction.commit().map_err(|_| {
                anyhow::anyhow!("Could not save the speed collection setting; retry turning it off")
            })?;
            // Keep historical samples until normal retention expires; disabling
            // collection never deletes token usage or other agents' history.
            self.stop_if_unused(&connection)?;
        }
        self.status().map_err(|_| anyhow::anyhow!("The setting was saved, but collection status could not be read; refresh the status"))
    }

    fn stop_if_unused(&self, connection: &rusqlite::Connection) -> Result<()> {
        if !SOURCES
            .into_iter()
            .map(|agent| is_enabled(connection, agent))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .any(|enabled| enabled)
        {
            if let Some(worker) = self
                .worker
                .lock()
                .map_err(|_| anyhow::anyhow!("speed collector unavailable"))?
                .take()
            {
                worker.stop();
            }
        }
        Ok(())
    }

    fn start(&self) -> Result<()> {
        self.start_on_port(PORT)
    }

    fn start_on_port(&self, port: u16) -> Result<()> {
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| anyhow::anyhow!("speed collector unavailable"))?;
        if worker
            .as_ref()
            .is_some_and(|worker| !worker.thread.is_finished())
        {
            return Ok(());
        }
        if let Some(stopped) = worker.take() {
            stopped.stop();
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .map_err(|_| anyhow::anyhow!(crate::i18n::tr!("本机速度采集端口不可用，请关闭其它 Metrik 实例后重试。", "The local speed collection port is unavailable. Close other Metrik instances and retry.")))?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let database = self.database.clone();
        let config = Arc::clone(&self.config);
        let thread = std::thread::Builder::new().name("metrik-response-speed".into()).spawn(move || {
            let mut decoder = Decoder::default();
            for incoming in listener.incoming() {
                if thread_stop.load(Ordering::Acquire) {break;}
                let Ok(mut stream) = incoming else {break;};
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                let response = handle_export(&mut stream, address.port(), &database, &config, &mut decoder);
                let (code, content_type) = match response {
                    Ok(content_type) => (200, content_type),
                    Err(code) => (code, "application/json".to_owned()),
                };
                let body = if code == 200 && content_type == "application/x-protobuf" { "" } else { "{}" };
                let reason = if code == 200 {"OK"} else {"Rejected"};
                let _ = write!(stream,"HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            }
        }).context("could not start the local speed collector")?;
        *worker = Some(Worker {
            stop,
            address,
            thread,
        });
        Ok(())
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                worker.stop();
            }
        }
    }
}

fn is_enabled(connection: &rusqlite::Connection, agent: &str) -> Result<bool> {
    Ok(storage::get_app_setting(connection, &setting(agent))?.as_deref() == Some("1"))
}

struct ExportRequest {
    agent: &'static str,
    signal: &'static str,
    content_type: String,
    body: Vec<u8>,
}

fn read_export(stream: &mut TcpStream, port: u16) -> std::result::Result<ExportRequest, u16> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut buffer = Vec::new();
    let header_end = loop {
        if buffer.len() > MAX_HEADERS {
            return Err(413);
        }
        if let Some(index) = buffer.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break index + 4;
        }
        read_more(stream, &mut buffer, deadline)?;
    };
    if header_end > MAX_HEADERS {
        return Err(413);
    }
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut headers);
    if !request
        .parse(&buffer[..header_end])
        .map_err(|_| 400_u16)?
        .is_complete()
    {
        return Err(400);
    }
    if request.method != Some("POST") {
        return Err(405);
    }
    let (agent, signal) = match request.path {
        Some("/claude/v1/logs") => ("claude", "logs"),
        Some("/codex/v1/traces") => ("codex", "traces"),
        _ => return Err(404),
    };
    let header = |key: &str| -> std::result::Result<Option<&str>, u16> {
        let values = request
            .headers
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case(key))
            .collect::<Vec<_>>();
        if values.len() > 1 {
            return Err(400);
        }
        values
            .first()
            .map(|h| {
                std::str::from_utf8(h.value)
                    .map(str::trim)
                    .map_err(|_| 400_u16)
            })
            .transpose()
    };
    let expected_host = format!("127.0.0.1:{port}");
    if header("Host")? != Some(expected_host.as_str()) || header("Origin")?.is_some() {
        return Err(403);
    }
    if header("Transfer-Encoding")?.is_some() {
        return Err(400);
    }
    let length: usize = header("Content-Length")?
        .ok_or(411_u16)?
        .parse()
        .map_err(|_| 400_u16)?;
    if length > MAX_BODY {
        return Err(413);
    }
    let content_type = header("Content-Type")?
        .ok_or(415_u16)?
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        content_type.as_str(),
        "application/json" | "application/x-protobuf"
    ) {
        return Err(415);
    }
    let gzip = match header("Content-Encoding")? {
        None | Some("identity") => false,
        Some("gzip") => true,
        _ => return Err(415),
    };
    while buffer.len() - header_end < length {
        read_more(stream, &mut buffer, deadline)?;
    }
    let raw = &buffer[header_end..header_end + length];
    let body = if gzip {
        let mut decoded = Vec::new();
        GzDecoder::new(raw)
            .take((MAX_BODY + 1) as u64)
            .read_to_end(&mut decoded)
            .map_err(|_| 400_u16)?;
        if decoded.len() > MAX_BODY {
            return Err(413);
        }
        decoded
    } else {
        raw.to_vec()
    };
    Ok(ExportRequest {
        agent,
        signal,
        content_type,
        body,
    })
}

fn read_more(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    deadline: Instant,
) -> std::result::Result<(), u16> {
    if Instant::now() > deadline {
        return Err(408);
    }
    let mut chunk = [0_u8; 8192];
    let count = stream.read(&mut chunk).map_err(|_| 408_u16)?;
    if count == 0 {
        return Err(400);
    }
    buffer.extend_from_slice(&chunk[..count]);
    if buffer.len() > MAX_BODY + MAX_HEADERS {
        return Err(413);
    }
    Ok(())
}

fn handle_export(
    stream: &mut TcpStream,
    port: u16,
    database: &std::path::Path,
    config: &ConfigManager,
    decoder: &mut Decoder,
) -> std::result::Result<String, u16> {
    let request = read_export(stream, port)?;
    // Startup owns schema migration. An export must not repeat schema writes
    // or recreate a removed ledger while the normal scanner is working.
    let mut connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .map_err(|_| 503_u16)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(|_| 503_u16)?;
    if !is_enabled(&connection, request.agent).map_err(|_| 503_u16)? {
        return Err(403);
    }
    let status = config
        .inspect(request.agent, &endpoint(request.agent))
        .map_err(|_| 503_u16)?;
    if !status.configured || status.conflict {
        return Err(403);
    }
    // Decoding caches cross-batch spans and replay IDs. Advance it only when
    // the database transaction succeeds, so a retried export remains usable.
    let mut candidate = decoder.clone();
    let samples = candidate
        .decode(
            request.agent,
            request.signal,
            &request.content_type,
            &request.body,
        )
        .map_err(|_| 400_u16)?;
    speed_store::store_samples(&mut connection, &samples, Utc::now().timestamp_millis())
        .map_err(|_| 503_u16)?;
    *decoder = candidate;
    Ok(request.content_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn collector_fixture() -> (tempfile::TempDir, Collector) {
        let temporary = tempfile::tempdir().unwrap();
        let database = temporary.path().join("usage.sqlite3");
        storage::open_database(&database).unwrap();
        let config = Arc::new(ConfigManager::with_paths(
            temporary.path().join("state"),
            temporary.path().join("claude.json"),
            temporary.path().join("codex.toml"),
        ));
        let collector = Collector {
            database,
            config,
            worker: Mutex::new(None),
        };
        (temporary, collector)
    }

    #[test]
    fn independent_toggles_restore_configs_and_database_failure_keeps_prior_installation() {
        let (_temporary, collector) = collector_fixture();
        assert!(!collector.status().unwrap().running);
        assert!(collector
            .status()
            .unwrap()
            .sources
            .iter()
            .all(|source| !source.enabled));
        // Tests use an ephemeral listener, never the user's production port.
        collector.start_on_port(0).unwrap();
        collector.set_enabled("claude", true).unwrap();
        collector.set_enabled("codex", true).unwrap();
        let connection = storage::open_database(&collector.database).unwrap();
        connection.execute_batch("CREATE TRIGGER refuse_settings BEFORE INSERT ON app_setting BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
        for enabled in [false, true] {
            assert!(collector.set_enabled("claude", enabled).is_err());
            assert!(is_enabled(&connection, "claude").unwrap());
            assert!(
                collector
                    .config
                    .inspect("claude", &endpoint("claude"))
                    .unwrap()
                    .configured
            );
        }
        connection
            .execute_batch("DROP TRIGGER refuse_settings;")
            .unwrap();
        let status = collector.set_enabled("claude", false).unwrap();
        assert!(status.running);
        assert!(!status.sources[0].enabled);
        assert!(status.sources[1].enabled);
        assert!(
            !collector
                .config
                .inspect("claude", &endpoint("claude"))
                .unwrap()
                .configured
        );
        assert!(!collector.set_enabled("codex", false).unwrap().running);
        assert!(
            !collector
                .config
                .inspect("codex", &endpoint("codex"))
                .unwrap()
                .configured
        );
    }

    #[test]
    fn failed_database_write_does_not_consume_codex_replay_ids() {
        let (_temporary, collector) = collector_fixture();
        collector
            .config
            .enable("codex", &endpoint("codex"))
            .unwrap();
        let connection = storage::open_database(&collector.database).unwrap();
        storage::set_app_setting(&connection, &setting("codex"), "1").unwrap();
        connection.execute_batch("CREATE TRIGGER refuse_samples BEFORE INSERT ON response_speed_sample BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
        let base = (Utc::now().timestamp_millis() as u64 - 5000) * 1_000_000;
        let span = |id: u8,
                    parent: Option<u8>,
                    name: &str,
                    start: u64,
                    end: u64,
                    attributes: serde_json::Value| json!({"traceId":"11111111111111111111111111111111","spanId":format!("{id:016x}"),"parentSpanId":parent.map(|id|format!("{id:016x}")).unwrap_or_default(),"name":name,"startTimeUnixNano":(base+start*1_000_000).to_string(),"endTimeUnixNano":(base+end*1_000_000).to_string(),"attributes":attributes});
        let spans = vec![
            span(
                1,
                None,
                "try_run_sampling_request",
                0,
                5000,
                json!([{"key":"model","value":{"stringValue":"gpt-test"}}]),
            ),
            span(2, Some(1), "codex.sampling", 100, 2100, json!([])),
            span(3, Some(1), "receiving_stream", 200, 4900, json!([])),
            span(
                4,
                Some(3),
                "handle_responses",
                2000,
                2099,
                json!([{"key":"gen_ai.usage.output_tokens","value":{"intValue":"40"}}]),
            ),
        ];
        let body = serde_json::to_vec(&json!({"resourceSpans":[{"scopeSpans":[{"spans":spans}]}]}))
            .unwrap();
        let mut bytes=format!("POST /codex/v1/traces HTTP/1.1\r\nHost: 127.0.0.1:46319\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",body.len()).into_bytes();
        bytes.extend_from_slice(&body);
        let config = Arc::clone(&collector.config);
        let database = collector.database.clone();
        let (result, mut decoder) = exchange(bytes.clone(), move |stream| {
            let mut decoder = Decoder::default();
            let result = handle_export(stream, PORT, &database, &config, &mut decoder);
            (result, decoder)
        });
        assert_eq!(result.unwrap_err(), 503);
        connection
            .execute_batch("DROP TRIGGER refuse_samples;")
            .unwrap();
        let config = Arc::clone(&collector.config);
        let database = collector.database.clone();
        let result = exchange(bytes, move |stream| {
            handle_export(stream, PORT, &database, &config, &mut decoder)
        });
        assert!(result.is_ok());
        let timings = speed_store::model_timings(&connection, 0, i64::MAX).unwrap();
        assert_eq!(timings.len(), 1);
        assert_eq!(timings[0].output_tokens, 40.0);
        assert_eq!(timings[0].duration_ms, 2000.0);
    }

    fn request(body: &[u8], headers: &str) -> Vec<u8> {
        let mut bytes = format!("POST /claude/v1/logs HTTP/1.1\r\nHost: 127.0.0.1:46319\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}\r\n", body.len()).into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    fn exchange<T: Send + 'static>(
        bytes: Vec<u8>,
        handler: impl FnOnce(&mut TcpStream) -> T + Send + 'static,
    ) -> T {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            handler(&mut stream)
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(&bytes).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        thread.join().unwrap()
    }

    #[test]
    fn ingress_rejects_browser_origins_foreign_hosts_and_ambiguous_headers() {
        for (headers, expected) in [
            ("Origin: http://example.invalid\r\n", 403),
            ("Host: example.invalid\r\n", 400),
            ("Transfer-Encoding: chunked\r\n", 400),
            ("Content-Length: 2\r\n", 400),
        ] {
            let result = exchange(request(b"{}", headers), |stream| {
                read_export(stream, PORT).err().unwrap()
            });
            assert_eq!(result, expected);
        }
        let mut foreign = request(b"{}", "");
        foreign = String::from_utf8(foreign)
            .unwrap()
            .replace("127.0.0.1:46319", "localhost:46319")
            .into_bytes();
        assert_eq!(
            exchange(foreign, |stream| read_export(stream, PORT).err().unwrap()),
            403
        );
    }

    #[test]
    fn gzip_body_is_bounded_after_decompression_and_truncated_requests_are_rejected() {
        use flate2::{write::GzEncoder, Compression};
        let compressed = |body: &[u8]| {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(body).unwrap();
            encoder.finish().unwrap()
        };
        let body = compressed(b"{\"resourceLogs\":[]}");
        let decoded = exchange(request(&body, "Content-Encoding: gzip\r\n"), |stream| {
            read_export(stream, PORT).unwrap().body
        });
        assert_eq!(decoded, b"{\"resourceLogs\":[]}");
        let oversized = compressed(&vec![b' '; MAX_BODY + 1]);
        assert_eq!(
            exchange(
                request(&oversized, "Content-Encoding: gzip\r\n"),
                |stream| read_export(stream, PORT).err().unwrap()
            ),
            413
        );
        let mut truncated = request(b"{}", "");
        truncated.pop();
        assert_eq!(
            exchange(truncated, |stream| read_export(stream, PORT).err().unwrap()),
            400
        );
    }

    #[test]
    fn enabled_export_is_idempotent_and_never_writes_usage_or_raw_payloads() {
        let temporary = tempfile::tempdir().unwrap();
        let database = temporary.path().join("usage.sqlite3");
        let config = Arc::new(ConfigManager::with_paths(
            temporary.path().join("state"),
            temporary.path().join("claude.json"),
            temporary.path().join("codex.toml"),
        ));
        config.enable("claude", &endpoint("claude")).unwrap();
        let connection = storage::open_database(&database).unwrap();
        storage::set_app_setting(&connection, &setting("claude"), "1").unwrap();
        let now = Utc::now().timestamp_millis();
        let attributes = [
            ("event.name", "api_request"),
            ("model", "claude-test"),
            ("request_id", "synthetic-request"),
            ("output_tokens", "120"),
            ("duration_ms", "1500"),
            ("prompt", "synthetic-private-content"),
        ]
        .into_iter()
        .map(|(key, value)| json!({"key":key,"value":{"stringValue":value}}))
        .collect::<Vec<_>>();
        let body=serde_json::to_vec(&json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[{"timeUnixNano":(now as u64*1_000_000).to_string(),"body":{"stringValue":"claude_code.api_request"},"attributes":attributes}]}]}]})).unwrap();
        let bytes = request(&body, "");
        for _ in 0..2 {
            let db = database.clone();
            let config = Arc::clone(&config);
            let result = exchange(bytes.clone(), move |stream| {
                handle_export(stream, PORT, &db, &config, &mut Decoder::default())
            });
            assert_eq!(result.unwrap(), "application/json");
        }
        assert_eq!(
            speed_store::source_counts(&connection, "claude", now + 1).unwrap(),
            (1, Some(now))
        );
        let usage: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage, 0);
        let model: String = connection
            .query_row("SELECT model FROM response_speed_sample", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(model, "claude-test");
        storage::set_app_setting(&connection, &setting("claude"), "0").unwrap();
        let db = database.clone();
        let disabled = exchange(bytes, move |stream| {
            handle_export(stream, PORT, &db, &config, &mut Decoder::default())
        });
        assert_eq!(disabled.unwrap_err(), 403);
        assert_eq!(
            speed_store::source_counts(&connection, "claude", now + 1)
                .unwrap()
                .0,
            1
        );
    }
}
