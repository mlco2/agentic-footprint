use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use af_events::{
    ActionSpan, AgentApp, Attribution, Collector, Envelope, ExecutionLocus, LlmCall, Payload,
    SessionMeta, Status, ToolKind, Usage, UsageSource,
};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const COLLECTOR_NAME: &str = "opencode";
const COLLECTOR_VERSION: &str = "0.1.0";
const CURSOR_VERSION: u8 = 1;
const RECONNECT_INITIAL: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(10);
const RECONNECT_JITTER: f64 = 0.2;

#[derive(Debug, Clone, clap::Args)]
pub struct Args {
    /// OpenCode session ID to collect.
    #[arg(long)]
    session_id: String,
    /// OpenCode server base URL.
    #[arg(long, default_value = "http://127.0.0.1:4096")]
    url: String,
    /// Project directory forwarded as x-opencode-directory.
    #[arg(long)]
    directory: Option<String>,
    /// Read a finite SSE fixture instead of connecting to a server.
    #[arg(long)]
    input: Option<PathBuf>,
    /// Override the saved exclusive durable sequence cursor.
    #[arg(long)]
    after: Option<u64>,
    /// Root PID attached to local action spans.
    #[arg(long)]
    pid: Option<i64>,
    /// OpenCode version recorded in session metadata.
    #[arg(long)]
    opencode_version: Option<String>,
    /// Do not emit session metadata.
    #[arg(long)]
    no_session_meta: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct SourceEvent {
    id: Option<String>,
    #[serde(rename = "type")]
    event_type: Option<String>,
    durable: Option<Durable>,
    #[serde(default)]
    data: Value,
}

#[derive(Debug, Clone, Deserialize)]
struct Durable {
    #[serde(rename = "aggregateID")]
    aggregate_id: Value,
    seq: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct NormalizerState {
    steps: BTreeMap<String, Value>,
    tools: BTreeMap<String, Value>,
    shells: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
struct Normalizer {
    pid: Option<i64>,
    state: NormalizerState,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorState {
    version: u8,
    server: String,
    session_id: String,
    after: u64,
    normalizer: NormalizerState,
}

pub fn run(state_dir: &Path, args: Args) -> Result<()> {
    let spool = spool_path(state_dir, &args.session_id);

    if let Some(input) = args.input.as_deref() {
        emit_session_meta(&spool, &args)?;
        let mut normalizer = Normalizer::new(args.pid, NormalizerState::default());
        let mut latest = args.after.unwrap_or(0);
        let file = File::open(input)
            .with_context(|| format!("open OpenCode SSE fixture {}", input.display()))?;
        collect_reader(
            BufReader::new(file),
            &mut normalizer,
            &spool,
            &args.session_id,
            &mut latest,
            None,
        )?;
        println!("{latest}");
        return Ok(());
    }

    let cursor = cursor_path(state_dir, &args.url, &args.session_id);
    let (latest, normalizer_state) =
        initial_state(&cursor, &args.url, &args.session_id, args.after)?;

    emit_session_meta(&spool, &args)?;
    collect_live(
        &args,
        &spool,
        &cursor,
        latest,
        Normalizer::new(args.pid, normalizer_state),
    )
}

fn initial_state(
    cursor: &Path,
    server: &str,
    session_id: &str,
    after: Option<u64>,
) -> Result<(u64, NormalizerState)> {
    if let Some(after) = after {
        return Ok((after, NormalizerState::default()));
    }
    Ok(match load_cursor(cursor, server, session_id)? {
        Some(saved) => (saved.after, saved.normalizer),
        None => (0, NormalizerState::default()),
    })
}

fn emit_session_meta(spool: &Path, args: &Args) -> Result<()> {
    if args.no_session_meta || spool.exists() {
        return Ok(());
    }
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("format session metadata timestamp")?;
    let envelope = Envelope {
        schema_version: "0.1.0".into(),
        event_id: format!("opencode-session-{}", digest_prefix(&args.session_id, 24)),
        ts: now,
        collector: collector(),
        session_id: args.session_id.clone(),
        attribution: None,
        payload: Payload::SessionMeta(SessionMeta {
            agent_app: AgentApp {
                name: "opencode".into(),
                version: args.opencode_version.clone(),
            },
            os: None,
            hardware: None,
            geo_zone: None,
            power_source: None,
        }),
    };
    append_envelopes(spool, std::slice::from_ref(&envelope))
}

fn collect_live(
    args: &Args,
    spool: &Path,
    cursor: &Path,
    mut latest: u64,
    mut normalizer: Normalizer,
) -> Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .build();
    let mut backoff = RECONNECT_INITIAL;

    loop {
        let before = latest;
        let url = stream_url(&args.url, &args.session_id, latest);
        let mut request = agent.get(&url).set("accept", "text/event-stream");
        if let Some(directory) = args.directory.as_deref() {
            request = request.set("x-opencode-directory", directory);
        }

        let result = request
            .call()
            .map_err(anyhow::Error::from)
            .and_then(|response| {
                collect_reader(
                    BufReader::new(response.into_reader()),
                    &mut normalizer,
                    spool,
                    &args.session_id,
                    &mut latest,
                    Some(CursorTarget {
                        path: cursor,
                        server: &args.url,
                    }),
                )
            });

        match result {
            Ok(()) => {
                eprintln!("af[opencode] info: stream ended after sequence {latest}");
            }
            Err(error) => {
                eprintln!(
                    "af[opencode] warn: stream disconnected after sequence {latest} ({error:#})"
                );
            }
        }

        if latest > before {
            backoff = RECONNECT_INITIAL;
        }

        let delay = jittered(backoff);
        eprintln!(
            "af[opencode] info: reconnecting from sequence {latest} in {:.2}s",
            delay.as_secs_f64()
        );
        thread::sleep(delay);
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

#[derive(Clone, Copy)]
struct CursorTarget<'a> {
    path: &'a Path,
    server: &'a str,
}

fn collect_reader<R: BufRead>(
    mut reader: R,
    normalizer: &mut Normalizer,
    spool: &Path,
    session_id: &str,
    latest: &mut u64,
    cursor: Option<CursorTarget<'_>>,
) -> Result<()> {
    let mut data = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .context("read OpenCode SSE stream")?;
        if read == 0 {
            if !data.is_empty() {
                *latest = process_frame(&data, normalizer, spool, session_id, *latest, cursor)?;
            }
            return Ok(());
        }

        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !data.is_empty() {
                *latest = process_frame(&data, normalizer, spool, session_id, *latest, cursor)?;
                data.clear();
            }
        } else if !line.starts_with(':') {
            if let Some(value) = line.strip_prefix("data:") {
                data.push(value.strip_prefix(' ').unwrap_or(value).to_string());
            }
        }
    }
}

fn process_frame(
    data: &[String],
    normalizer: &mut Normalizer,
    spool: &Path,
    session_id: &str,
    latest: u64,
    cursor: Option<CursorTarget<'_>>,
) -> Result<u64> {
    let event: SourceEvent = match serde_json::from_str(&data.join("\n")) {
        Ok(event) => event,
        Err(error) => {
            eprintln!("af[opencode] warn: skipped unparseable SSE frame ({error})");
            return Ok(latest);
        }
    };
    process_event(event, normalizer, spool, session_id, latest, cursor)
}

fn process_event(
    event: SourceEvent,
    normalizer: &mut Normalizer,
    spool: &Path,
    session_id: &str,
    latest: u64,
    cursor: Option<CursorTarget<'_>>,
) -> Result<u64> {
    let sequence = match durable_sequence(&event, session_id) {
        DurableSequence::OtherSession => return Ok(latest),
        DurableSequence::Malformed(reason) => {
            eprintln!("af[opencode] warn: skipped durable event ({reason})");
            return Ok(latest);
        }
        DurableSequence::None => None,
        DurableSequence::Sequence(sequence) => Some(sequence),
    };

    if let Some(sequence) = sequence {
        if sequence <= latest {
            eprintln!(
                "af[opencode] warn: sequence regression for {session_id}: received {sequence} after {latest}; skipped replayed event"
            );
            return Ok(latest);
        }
        if sequence > latest + 1 {
            eprintln!(
                "af[opencode] warn: sequence gap for {session_id}: expected {}, received {sequence}",
                latest + 1
            );
        }
    }

    let state_before = normalizer.state.clone();
    let envelopes = match normalizer.normalize(&event) {
        Ok(envelopes) => envelopes,
        Err(error) => {
            normalizer.state = state_before.clone();
            eprintln!(
                "af[opencode] warn: skipped {} ({error:#})",
                event.event_type.as_deref().unwrap_or("malformed event")
            );
            Vec::new()
        }
    };
    if let Err(error) = append_envelopes(spool, &envelopes) {
        normalizer.state = state_before;
        return Err(error);
    }

    if let Some(sequence) = sequence {
        if let Some(target) = cursor {
            if let Err(error) = save_cursor(
                target.path,
                target.server,
                session_id,
                sequence,
                &normalizer.state,
            ) {
                normalizer.state = state_before;
                return Err(error);
            }
        }
        Ok(sequence)
    } else {
        Ok(latest)
    }
}

enum DurableSequence {
    None,
    OtherSession,
    Sequence(u64),
    Malformed(&'static str),
}

fn durable_sequence(event: &SourceEvent, session_id: &str) -> DurableSequence {
    let Some(durable) = event.durable.as_ref() else {
        return DurableSequence::None;
    };
    let Some(aggregate_id) = durable.aggregate_id.as_str() else {
        return DurableSequence::Malformed("aggregateID is not a string");
    };
    if aggregate_id != session_id {
        return DurableSequence::OtherSession;
    }
    let Some(sequence) = durable.seq.as_u64() else {
        return DurableSequence::Malformed("seq is not a positive integer");
    };
    if sequence == 0 {
        return DurableSequence::Malformed("seq is not a positive integer");
    }
    DurableSequence::Sequence(sequence)
}

impl Normalizer {
    fn new(pid: Option<i64>, state: NormalizerState) -> Self {
        Self { pid, state }
    }

    fn normalize(&mut self, event: &SourceEvent) -> Result<Vec<Envelope>> {
        let event_type = event.event_type.as_deref().unwrap_or_default();
        let data = event
            .data
            .as_object()
            .ok_or_else(|| anyhow!("data is not an object"))?;
        required_u64(data, "timestamp")?;
        required_str(data, "sessionID")?;

        match event_type {
            "session.next.step.started" => {
                self.state.steps.insert(
                    required_str(data, "assistantMessageID")?.to_string(),
                    event.data.clone(),
                );
                Ok(Vec::new())
            }
            "session.next.step.ended" | "session.next.step.failed" => {
                self.step(event, event_type.ends_with("failed"))
            }
            "session.next.tool.called" => {
                self.state.tools.insert(
                    required_str(data, "callID")?.to_string(),
                    event.data.clone(),
                );
                Ok(Vec::new())
            }
            "session.next.tool.success" | "session.next.tool.failed" => {
                self.tool(event, event_type.ends_with("failed"))
            }
            "session.next.shell.started" => {
                self.state.shells.insert(
                    required_str(data, "callID")?.to_string(),
                    event.data.clone(),
                );
                Ok(Vec::new())
            }
            "session.next.shell.ended" => self.shell(event),
            _ => Ok(Vec::new()),
        }
    }

    fn step(&mut self, event: &SourceEvent, failed: bool) -> Result<Vec<Envelope>> {
        let data = object(&event.data)?;
        let message_id = required_str(data, "assistantMessageID")?;
        let Some(started) = self.state.steps.remove(message_id) else {
            eprintln!(
                "af[opencode] warn: skipped {} without replayed step start",
                event.event_type.as_deref().unwrap_or("step settlement")
            );
            return Ok(Vec::new());
        };
        let started = object(&started)?;
        let model = object(required_value(started, "model")?)?;
        let tokens = data.get("tokens").and_then(Value::as_object);
        let cache = tokens
            .and_then(|tokens| tokens.get("cache"))
            .and_then(Value::as_object);
        let usage = Usage {
            input_tokens: optional_u64(tokens, "input"),
            output_tokens: optional_u64(tokens, "output"),
            thought_tokens: optional_u64(tokens, "reasoning"),
            cached_read_tokens: optional_u64(cache, "read"),
            cached_write_tokens: optional_u64(cache, "write"),
        };
        let status = if failed {
            let cancelled = data
                .get("error")
                .is_some_and(|error| error.to_string().to_lowercase().contains("cancel"));
            Some(if cancelled {
                Status::Cancelled
            } else {
                Status::Error
            })
        } else {
            Some(Status::Ok)
        };
        let start_ms = required_u64(started, "timestamp")?;
        let end_ms = required_u64(data, "timestamp")?;
        let attribution = Attribution {
            agent_id: optional_str(started, "agent").map(str::to_string),
            task_id: Some(message_id.to_string()),
            ..Attribution::default()
        };
        Ok(vec![self.envelope(
            event,
            Some(attribution),
            Payload::LlmCall(LlmCall {
                provider: required_str(model, "providerID")?.to_string(),
                model_id_requested: required_str(model, "id")?.to_string(),
                model_id_served: None,
                endpoint: None,
                usage,
                usage_source: UsageSource::AgentTelemetry,
                duration_ms: Some(end_ms.saturating_sub(start_ms)),
                status,
                streaming: Some(true),
            }),
        )?])
    }

    fn tool(&mut self, event: &SourceEvent, failed: bool) -> Result<Vec<Envelope>> {
        let data = object(&event.data)?;
        let call_id = required_str(data, "callID")?;
        let Some(started) = self.state.tools.remove(call_id) else {
            eprintln!(
                "af[opencode] warn: skipped {} without replayed tool call",
                event.event_type.as_deref().unwrap_or("tool settlement")
            );
            return Ok(Vec::new());
        };
        let started = object(&started)?;
        let tool_name = required_str(started, "tool")?;
        let tool_kind = tool_kind(tool_name);
        let provider_executed = started
            .get("provider")
            .and_then(Value::as_object)
            .and_then(|provider| provider.get("executed"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let locus = execution_locus(tool_kind, provider_executed);
        let pids = self
            .pid
            .filter(|_| matches!(locus, ExecutionLocus::Local | ExecutionLocus::Hybrid))
            .map(|pid| vec![pid]);
        Ok(vec![self.envelope(
            event,
            Some(Attribution {
                task_id: Some(required_str(data, "assistantMessageID")?.to_string()),
                tool_call_id: Some(call_id.to_string()),
                ..Attribution::default()
            }),
            Payload::ActionSpan(ActionSpan {
                span_id: call_id.to_string(),
                tool_name: tool_name.to_string(),
                tool_kind,
                execution_locus: locus,
                t_start: timestamp(required_u64(started, "timestamp")?)?,
                t_end: timestamp(required_u64(data, "timestamp")?)?,
                pids,
                cgroup: None,
                status: Some(if failed { Status::Error } else { Status::Ok }),
            }),
        )?])
    }

    fn shell(&mut self, event: &SourceEvent) -> Result<Vec<Envelope>> {
        let data = object(&event.data)?;
        let call_id = required_str(data, "callID")?;
        let Some(started) = self.state.shells.remove(call_id) else {
            return Ok(Vec::new());
        };
        let started = object(&started)?;
        Ok(vec![self.envelope(
            event,
            Some(Attribution {
                tool_call_id: Some(call_id.to_string()),
                ..Attribution::default()
            }),
            Payload::ActionSpan(ActionSpan {
                span_id: call_id.to_string(),
                tool_name: "shell".into(),
                tool_kind: ToolKind::Bash,
                execution_locus: ExecutionLocus::Local,
                t_start: timestamp(required_u64(started, "timestamp")?)?,
                t_end: timestamp(required_u64(data, "timestamp")?)?,
                pids: self.pid.map(|pid| vec![pid]),
                cgroup: None,
                status: Some(Status::Ok),
            }),
        )?])
    }

    fn envelope(
        &self,
        event: &SourceEvent,
        attribution: Option<Attribution>,
        payload: Payload,
    ) -> Result<Envelope> {
        let data = object(&event.data)?;
        let source_id = event.id.as_deref().context("event id is missing")?;
        Ok(Envelope {
            schema_version: "0.1.0".into(),
            event_id: stable_event_id(source_id),
            ts: timestamp(required_u64(data, "timestamp")?)?,
            collector: collector(),
            session_id: required_str(data, "sessionID")?.to_string(),
            attribution,
            payload,
        })
    }
}

fn collector() -> Collector {
    Collector {
        name: COLLECTOR_NAME.into(),
        version: COLLECTOR_VERSION.into(),
    }
}

fn tool_kind(name: &str) -> ToolKind {
    match name.to_ascii_lowercase().as_str() {
        "bash" | "shell" | "terminal" => ToolKind::Bash,
        "edit" | "write" | "read" | "patch" | "multiedit" => ToolKind::FileOp,
        "task" | "agent" | "subagent" => ToolKind::Subagent,
        "webfetch" | "websearch" | "fetch" | "search" => ToolKind::Web,
        value if value.starts_with("mcp") => ToolKind::Mcp,
        _ => ToolKind::Other,
    }
}

fn execution_locus(kind: ToolKind, provider_executed: bool) -> ExecutionLocus {
    if provider_executed {
        return ExecutionLocus::Remote;
    }
    match kind {
        ToolKind::Bash | ToolKind::FileOp | ToolKind::Subagent => ExecutionLocus::Local,
        ToolKind::Web => ExecutionLocus::Remote,
        ToolKind::Mcp | ToolKind::Other => ExecutionLocus::Unknown,
    }
}

fn append_envelopes(path: &Path, envelopes: &[Envelope]) -> Result<()> {
    if envelopes.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create spool directory {}", parent.display()))?;
    }
    let mut bytes = Vec::new();
    for envelope in envelopes {
        serde_json::to_writer(&mut bytes, envelope).context("serialize OpenCode event")?;
        bytes.push(b'\n');
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open spool {}", path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("append spool {}", path.display()))
}

fn load_cursor(path: &Path, server: &str, session_id: &str) -> Result<Option<CursorState>> {
    let mut contents = String::new();
    match File::open(path) {
        Ok(mut file) => {
            file.read_to_string(&mut contents)
                .with_context(|| format!("read OpenCode cursor {}", path.display()))?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("open OpenCode cursor {}", path.display()))
        }
    }
    let cursor: CursorState = serde_json::from_str(&contents)
        .with_context(|| format!("invalid OpenCode cursor {}", path.display()))?;
    if cursor.version != CURSOR_VERSION
        || cursor.server != canonical_server(server)
        || cursor.session_id != session_id
    {
        bail!("invalid OpenCode cursor {}", path.display());
    }
    Ok(Some(cursor))
}

fn save_cursor(
    path: &Path,
    server: &str,
    session_id: &str,
    after: u64,
    normalizer: &NormalizerState,
) -> Result<()> {
    let parent = path.parent().context("OpenCode cursor has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create cursor directory {}", parent.display()))?;
    let temporary = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cursor"),
        std::process::id()
    ));
    let value = CursorState {
        version: CURSOR_VERSION,
        server: canonical_server(server).to_string(),
        session_id: session_id.to_string(),
        after,
        normalizer: normalizer.clone(),
    };
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create cursor temporary file {}", temporary.display()))?;
        serde_json::to_writer(&mut file, &value).context("serialize OpenCode cursor")?;
        file.write_all(b"\n").context("finish OpenCode cursor")?;
        file.sync_all().context("sync OpenCode cursor")?;
        fs::rename(&temporary, path)
            .with_context(|| format!("replace OpenCode cursor {}", path.display()))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("sync cursor directory {}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn spool_path(state_dir: &Path, session_id: &str) -> PathBuf {
    state_dir.join("spool").join(af_spool::spool_file_name(
        COLLECTOR_NAME,
        &safe_component(session_id),
    ))
}

fn cursor_path(state_dir: &Path, server: &str, session_id: &str) -> PathBuf {
    state_dir.join("cursors").join(COLLECTOR_NAME).join(format!(
        "{}.{}.json",
        digest_prefix(canonical_server(server), 24),
        safe_component(session_id)
    ))
}

fn canonical_server(server: &str) -> &str {
    server.trim_end_matches('/')
}

fn stream_url(server: &str, session_id: &str, after: u64) -> String {
    format!(
        "{}/api/session/{}/event?after={after}",
        canonical_server(server),
        percent_encode_path(session_id)
    )
}

fn percent_encode_path(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn safe_component(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        value.to_string()
    } else {
        digest_prefix(value, 32)
    }
}

fn stable_event_id(value: &str) -> String {
    if value.len() >= 16 {
        value.to_string()
    } else {
        format!("opencode-{}", digest_prefix(value, 24))
    }
}

fn digest_prefix(value: &str, length: usize) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")[..length].to_string()
}

fn timestamp(epoch_ms: u64) -> Result<String> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(epoch_ms) * 1_000_000)
        .context("OpenCode timestamp is out of range")?
        .format(&Rfc3339)
        .context("format OpenCode timestamp")
}

fn jittered(backoff: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let unit = f64::from(nanos) / f64::from(u32::MAX);
    let multiplier = 1.0 - RECONNECT_JITTER + unit * RECONNECT_JITTER * 2.0;
    Duration::from_secs_f64(backoff.as_secs_f64() * multiplier)
}

fn object(value: &Value) -> Result<&serde_json::Map<String, Value>> {
    value.as_object().context("value is not an object")
}

fn required_value<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a Value> {
    object.get(key).with_context(|| format!("missing {key}"))
}

fn required_str<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    required_value(object, key)?
        .as_str()
        .with_context(|| format!("{key} is not a string"))
}

fn optional_str<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn required_u64(object: &serde_json::Map<String, Value>, key: &str) -> Result<u64> {
    required_value(object, key)?
        .as_u64()
        .with_context(|| format!("{key} is not an unsigned integer"))
}

fn optional_u64(object: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<u64> {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture() -> &'static [u8] {
        include_bytes!("../../../../collectors/opencode/test-data/session.sse")
    }

    #[test]
    fn offline_fixture_matches_contract() {
        let dir = tempfile::tempdir().unwrap();
        let spool = spool_path(dir.path(), "ses_fixture");
        let mut normalizer = Normalizer::new(Some(4242), NormalizerState::default());
        let mut latest = 0;
        collect_reader(
            BufReader::new(Cursor::new(fixture())),
            &mut normalizer,
            &spool,
            "ses_fixture",
            &mut latest,
            None,
        )
        .unwrap();
        assert_eq!(latest, 10);
        let lines = fs::read_to_string(spool).unwrap();
        assert_eq!(lines.lines().count(), 4);
        for line in lines.lines() {
            af_events::parse_line(line).unwrap();
        }
    }

    #[test]
    fn cursor_preserves_pending_start_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let spool = spool_path(dir.path(), "ses_fixture");
        let cursor = cursor_path(dir.path(), "http://127.0.0.1:4096", "ses_fixture");
        let frames: Vec<&[u8]> = fixture().split(|byte| *byte == b'\n').collect();
        let first = frames[..2].join(&b'\n');
        let mut normalizer = Normalizer::new(None, NormalizerState::default());
        let mut latest = 0;
        collect_reader(
            BufReader::new(Cursor::new(first)),
            &mut normalizer,
            &spool,
            "ses_fixture",
            &mut latest,
            Some(CursorTarget {
                path: &cursor,
                server: "http://127.0.0.1:4096",
            }),
        )
        .unwrap();
        assert_eq!(latest, 1);
        let saved = load_cursor(&cursor, "http://127.0.0.1:4096", "ses_fixture")
            .unwrap()
            .unwrap();
        assert_eq!(saved.after, 1);
        assert_eq!(saved.normalizer.steps.len(), 1);
    }

    #[test]
    fn reconnect_replay_does_not_duplicate_facts() {
        let dir = tempfile::tempdir().unwrap();
        let spool = spool_path(dir.path(), "ses");
        let cursor = cursor_path(dir.path(), "http://server", "ses");
        let started = r#"data: {"id":"evt_started_123456","type":"session.next.step.started","durable":{"aggregateID":"ses","seq":1},"data":{"timestamp":1,"sessionID":"ses","assistantMessageID":"msg","agent":"build","model":{"providerID":"provider","id":"model"}}}

"#;
        let ended = r#"data: {"id":"evt_ended_12345678","type":"session.next.step.ended","durable":{"aggregateID":"ses","seq":2},"data":{"timestamp":2,"sessionID":"ses","assistantMessageID":"msg","tokens":{"input":1,"output":1}}}

"#;
        let mut latest = 0;
        let mut normalizer = Normalizer::new(None, NormalizerState::default());
        collect_reader(
            BufReader::new(Cursor::new(started)),
            &mut normalizer,
            &spool,
            "ses",
            &mut latest,
            Some(CursorTarget {
                path: &cursor,
                server: "http://server",
            }),
        )
        .unwrap();
        assert_eq!(latest, 1);

        let saved = load_cursor(&cursor, "http://server", "ses")
            .unwrap()
            .unwrap();
        let mut resumed = Normalizer::new(None, saved.normalizer);
        let replay = format!("{started}{ended}");
        collect_reader(
            BufReader::new(Cursor::new(replay)),
            &mut resumed,
            &spool,
            "ses",
            &mut latest,
            Some(CursorTarget {
                path: &cursor,
                server: "http://server",
            }),
        )
        .unwrap();

        assert_eq!(latest, 2);
        let lines = fs::read_to_string(spool).unwrap();
        assert_eq!(lines.lines().count(), 1);
        let event = af_events::parse_line(lines.trim()).unwrap();
        assert_eq!(event.type_tag(), "llm_call");
    }

    #[test]
    fn corrupt_cursor_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = cursor_path(dir.path(), "http://127.0.0.1:4096", "ses_fixture");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not json").unwrap();
        let error = load_cursor(&path, "http://127.0.0.1:4096", "ses_fixture").unwrap_err();
        assert!(error.to_string().contains("invalid OpenCode cursor"));
    }

    #[test]
    fn explicit_after_does_not_require_reading_saved_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = cursor_path(dir.path(), "http://127.0.0.1:4096", "ses_fixture");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not json").unwrap();

        let (latest, state) =
            initial_state(&path, "http://127.0.0.1:4096", "ses_fixture", Some(0)).unwrap();
        assert_eq!(latest, 0);
        assert!(state.steps.is_empty());
    }

    #[test]
    fn cursor_is_written_after_spool_append() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool");
        fs::create_dir(&spool).unwrap();
        let cursor = dir.path().join("cursor.json");
        let started: SourceEvent = serde_json::from_str(
            r#"{"id":"evt_1234567890123456","type":"session.next.step.started","durable":{"aggregateID":"ses","seq":1},"data":{"timestamp":1,"sessionID":"ses","assistantMessageID":"msg","model":{"providerID":"p","id":"m"}}}"#,
        )
        .unwrap();
        let mut normalizer = Normalizer::new(None, NormalizerState::default());
        process_event(started, &mut normalizer, &spool, "ses", 0, None).unwrap();
        fs::create_dir_all(&spool).unwrap();
        let ended: SourceEvent = serde_json::from_str(
            r#"{"id":"evt_abcdefghijklmnop","type":"session.next.step.ended","durable":{"aggregateID":"ses","seq":2},"data":{"timestamp":2,"sessionID":"ses","assistantMessageID":"msg","tokens":{"input":1,"output":1}}}"#,
        )
        .unwrap();
        let error = process_event(
            ended,
            &mut normalizer,
            &spool,
            "ses",
            1,
            Some(CursorTarget {
                path: &cursor,
                server: "http://server",
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("open spool"));
        assert!(!cursor.exists());
        assert_eq!(normalizer.state.steps.len(), 1);
    }

    #[test]
    fn stream_url_encodes_session_and_cursor() {
        assert_eq!(
            stream_url("http://server/", "ses/a b", 42),
            "http://server/api/session/ses%2Fa%20b/event?after=42"
        );
    }
}
