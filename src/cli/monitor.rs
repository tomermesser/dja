use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Row, Table},
    Frame, Terminal,
};
use serde::Deserialize;
use std::io::stdout;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::Config;

// ---------------------------------------------------------------------------
// Data types mirroring the daemon's JSON responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
struct StatsData {
    hits: u64,
    misses: u64,
    skips: u64,
    errors: u64,
    time_saved_ms: u64,
    estimated_tokens_saved: u64,
    estimated_cost_saved_usd: f64,
    uptime_secs: u64,
    cache_entry_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct EventData {
    event_type: String,
    latency_ms: Option<u64>,
    prompt_snippet: Option<String>,
    model: Option<String>,
    #[allow(dead_code)]
    similarity: Option<f32>,
    #[allow(dead_code)]
    cache_id: Option<i64>,
    #[allow(dead_code)]
    body_size: usize,
    #[allow(dead_code)]
    response_size: Option<usize>,
    timestamp: String,
}

// ---------------------------------------------------------------------------
// Monitor state
// ---------------------------------------------------------------------------

struct MonitorState {
    stats: Option<StatsData>,
    events: Vec<EventData>, // newest first, max 100
    connected: bool,
}

impl MonitorState {
    fn new() -> Self {
        Self {
            stats: None,
            events: Vec::new(),
            connected: false,
        }
    }

    fn push_event(&mut self, event: EventData) {
        self.events.insert(0, event);
        self.events.truncate(100);
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub async fn run() -> Result<()> {
    let config = Config::load()?;
    let port = config.port;

    // Check if daemon is running by hitting the stats endpoint once.
    let stats_url = format!("http://127.0.0.1:{port}/internal/stats");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;

    match client.get(&stats_url).send().await {
        Ok(resp) if resp.status().is_success() => {}
        _ => {
            println!("dja daemon is not running. Start it with `dja start`.");
            return Ok(());
        }
    }

    // Set up terminal
    enable_raw_mode().context("enabling raw mode")?;
    stdout()
        .execute(EnterAlternateScreen)
        .context("entering alternate screen")?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("creating terminal")?;

    let result = run_monitor(&mut terminal, port).await;

    // Restore terminal
    disable_raw_mode().ok();
    stdout().execute(LeaveAlternateScreen).ok();

    result
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

async fn run_monitor(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    port: u16,
) -> Result<()> {
    let mut state = MonitorState::new();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let stats_url = format!("http://127.0.0.1:{port}/internal/stats");
    let events_url = format!("http://127.0.0.1:{port}/internal/events");

    // Channel for SSE events coming from the background task.
    let (event_tx, mut event_rx) = mpsc::channel::<EventData>(256);

    // Spawn SSE connector.
    let sse_events_url = events_url.clone();
    tokio::spawn(async move {
        connect_sse(sse_events_url, event_tx).await;
    });

    let mut render_interval = tokio::time::interval(Duration::from_millis(100));
    let mut stats_interval = tokio::time::interval(Duration::from_secs(2));

    // Do an initial stats fetch right away.
    if let Ok(resp) = client.get(&stats_url).send().await {
        if let Ok(data) = resp.json::<StatsData>().await {
            state.stats = Some(data);
            state.connected = true;
        }
    }

    loop {
        tokio::select! {
            _ = render_interval.tick() => {
                // Check for keyboard events (non-blocking).
                while event::poll(Duration::ZERO)? {
                    if let Event::Key(key) = event::read()? {
                        if key.code == KeyCode::Char('q')
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL))
                        {
                            return Ok(());
                        }
                    }
                }
                terminal.draw(|f| render(f, &state))?;
            }
            _ = stats_interval.tick() => {
                match client.get(&stats_url).send().await {
                    Ok(resp) => {
                        if let Ok(data) = resp.json::<StatsData>().await {
                            state.stats = Some(data);
                            state.connected = true;
                        }
                    }
                    Err(_) => {
                        state.connected = false;
                    }
                }
            }
            Some(ev) = event_rx.recv() => {
                state.push_event(ev);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SSE connector — reconnects on failure
// ---------------------------------------------------------------------------

async fn connect_sse(url: String, tx: mpsc::Sender<EventData>) {
    let client = reqwest::Client::new();

    loop {
        match client.get(&url).send().await {
            Ok(resp) => {
                use futures::StreamExt;
                let mut stream = resp.bytes_stream();
                let mut buf = String::new();

                while let Some(chunk) = stream.next().await {
                    let chunk = match chunk {
                        Ok(c) => c,
                        Err(_) => break,
                    };
                    buf.push_str(&String::from_utf8_lossy(&chunk));

                    // Parse complete SSE messages (terminated by double newline).
                    while let Some(pos) = buf.find("\n\n") {
                        let message = buf[..pos].to_string();
                        buf = buf[pos + 2..].to_string();

                        // Extract the data line from the SSE message.
                        if let Some(data) = extract_sse_data(&message) {
                            if let Ok(event) = serde_json::from_str::<EventData>(&data) {
                                if tx.send(event).await.is_err() {
                                    return; // receiver dropped
                                }
                            }
                        }
                    }
                }
            }
            Err(_) => {}
        }

        // Wait before reconnecting.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Extract the `data:` field from an SSE message block.
fn extract_sse_data(message: &str) -> Option<String> {
    for line in message.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(f: &mut Frame, state: &MonitorState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(6), // stats panel
            Constraint::Min(3),   // live feed
        ])
        .split(f.area());

    render_header(f, chunks[0], state);
    render_stats(f, chunks[1], state);
    render_live_feed(f, chunks[2], state);
}

fn render_header(f: &mut Frame, area: Rect, state: &MonitorState) {
    let (uptime_str, cache_str) = match &state.stats {
        Some(s) => {
            let secs = s.uptime_secs;
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let uptime = if h > 0 {
                format!("{h}h {m:02}m")
            } else {
                format!("{m}m")
            };
            (uptime, format!("{}", s.cache_entry_count))
        }
        None => ("--".into(), "--".into()),
    };

    let status_dot = if state.connected {
        Span::styled("●", Style::default().fg(Color::Green))
    } else {
        Span::styled("●", Style::default().fg(Color::Red))
    };

    let header = Line::from(vec![
        Span::styled(" dja monitor ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        status_dot,
        Span::raw("  "),
        Span::styled(format!("uptime: {uptime_str}"), Style::default().fg(Color::White)),
        Span::raw("   "),
        Span::styled(format!("cache: {cache_str}"), Style::default().fg(Color::Cyan)),
    ]);

    f.render_widget(Paragraph::new(header), area);
}

fn render_stats(f: &mut Frame, area: Rect, state: &MonitorState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    match &state.stats {
        None => {
            let msg = Paragraph::new(Span::styled(
                " Connecting...",
                Style::default().fg(Color::DarkGray),
            ));
            f.render_widget(msg, inner);
        }
        Some(s) => {
            // Split stats into three columns.
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(30),
                    Constraint::Percentage(35),
                    Constraint::Percentage(35),
                ])
                .split(inner);

            // Column 1: Request counts
            let requests = Paragraph::new(vec![
                Line::from(Span::styled(
                    " Requests",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(vec![
                    Span::raw("  HIT:  "),
                    Span::styled(format!("{:>5}", s.hits), Style::default().fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::raw("  MISS: "),
                    Span::styled(format!("{:>5}", s.misses), Style::default().fg(Color::Yellow)),
                ]),
                Line::from(vec![
                    Span::raw("  SKIP: "),
                    Span::styled(format!("{:>5}", s.skips), Style::default().fg(Color::DarkGray)),
                ]),
            ]);
            f.render_widget(requests, cols[0]);

            // Column 2: Hit rate gauge
            let total = s.hits + s.misses + s.skips + s.errors;
            let rate = if total > 0 {
                s.hits as f64 / total as f64
            } else {
                0.0
            };

            let gauge_area = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // label
                    Constraint::Length(1), // gauge
                    Constraint::Length(1), // percentage text
                    Constraint::Min(0),
                ])
                .split(cols[1]);

            let label = Paragraph::new(Span::styled(
                " Hit Rate",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
            f.render_widget(label, gauge_area[0]);

            let gauge = Gauge::default()
                .gauge_style(Style::default().fg(Color::Green).bg(Color::DarkGray))
                .ratio(rate.min(1.0))
                .label("");
            f.render_widget(gauge, gauge_area[1]);

            let pct_text = Paragraph::new(Span::styled(
                format!(" {:.1}%", rate * 100.0),
                Style::default().fg(Color::White),
            ));
            f.render_widget(pct_text, gauge_area[2]);

            // Column 3: Savings
            let time_saved = format_time_saved(s.time_saved_ms);
            let tokens_saved = format_tokens(s.estimated_tokens_saved);
            let cost_saved = format!("${:.2}", s.estimated_cost_saved_usd);

            let savings = Paragraph::new(vec![
                Line::from(Span::styled(
                    " Savings",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(vec![
                    Span::raw("  Time:   "),
                    Span::styled(format!("{time_saved} saved"), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::raw("  Tokens: "),
                    Span::styled(format!("~{tokens_saved} est."), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::raw("  Cost:   "),
                    Span::styled(format!("~{cost_saved} est."), Style::default().fg(Color::Cyan)),
                ]),
            ]);
            f.render_widget(savings, cols[2]);
        }
    }
}

fn render_live_feed(f: &mut Frame, area: Rect, state: &MonitorState) {
    let block = Block::default()
        .title(Span::styled(
            " Live Requests ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if state.events.is_empty() {
        let msg = Paragraph::new(Span::styled(
            " Waiting for requests...",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(msg, inner);
        return;
    }

    let max_rows = inner.height as usize;
    let visible = &state.events[..state.events.len().min(max_rows)];

    let rows: Vec<Row> = visible
        .iter()
        .map(|ev| {
            let color = event_color(&ev.event_type);

            let time_str = format_timestamp(&ev.timestamp);
            let type_str = ev.event_type.to_uppercase();
            let latency_str = match ev.latency_ms {
                Some(ms) => format!("{ms}ms"),
                None => "--".into(),
            };
            let snippet = ev
                .prompt_snippet
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(50)
                .collect::<String>();
            let snippet_display = format!("\"{snippet}\"");
            let model_str = shorten_model(ev.model.as_deref().unwrap_or(""));

            Row::new(vec![
                time_str,
                type_str,
                latency_str,
                snippet_display,
                model_str,
            ])
            .style(Style::default().fg(color))
        })
        .collect();

    let widths = [
        Constraint::Length(8),  // time
        Constraint::Length(5),  // type
        Constraint::Length(8),  // latency
        Constraint::Min(20),   // snippet
        Constraint::Length(8),  // model
    ];

    let table = Table::new(rows, widths);
    f.render_widget(table, inner);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn event_color(event_type: &str) -> Color {
    match event_type.to_lowercase().as_str() {
        "hit" => Color::Green,
        "miss" => Color::Yellow,
        "skip" => Color::DarkGray,
        "error" => Color::Red,
        _ => Color::White,
    }
}

fn shorten_model(model: &str) -> String {
    if model.contains("opus") {
        "opus".into()
    } else if model.contains("sonnet") {
        "sonnet".into()
    } else if model.contains("haiku") {
        "haiku".into()
    } else if model.is_empty() {
        "--".into()
    } else {
        model.to_string()
    }
}

fn format_timestamp(ts: &str) -> String {
    if let Ok(secs) = ts.parse::<u64>() {
        // Convert unix timestamp to HH:MM:SS local time.
        let dt = std::time::UNIX_EPOCH + Duration::from_secs(secs);
        if let Ok(elapsed) = dt.elapsed() {
            // We have a past timestamp, compute what local time it was.
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let local_secs = secs; // This is already UTC epoch seconds.
            // Simple HH:MM:SS from epoch — we want local time.
            // Use a basic approach: get total seconds of day.
            let _ = elapsed; // suppress unused
            let _ = now_secs;
            // For simplicity, compute using libc localtime.
            return format_epoch_local(local_secs);
        }
        return format_epoch_local(secs);
    }
    ts.chars().take(8).collect()
}

fn format_epoch_local(epoch_secs: u64) -> String {
    let secs = epoch_secs as i64;
    let tm = unsafe {
        let mut result: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs as *const i64, &mut result);
        result
    };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn format_time_saved(ms: u64) -> String {
    let total_secs = ms / 1000;
    let minutes = total_secs / 60;
    let secs = total_secs % 60;
    if minutes > 0 {
        format!("{minutes}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}
