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
    #[serde(default)]
    coalesced: u64,
    time_saved_ms: u64,
    estimated_tokens_saved: u64,
    estimated_cost_saved_usd: f64,
    uptime_secs: u64,
    cache_entry_count: u64,
    #[serde(default)]
    p2p_hits: u64,
    #[serde(default)]
    p2p_served: u64,
    #[serde(default)]
    p2p_errors: u64,
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
    /// Hostname that originally cached this response (only set on cache hits).
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FriendEntry {
    peer_id: String,
    display_name: String,
    public_addr: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct FriendsData {
    friends: Vec<FriendEntry>,
}

// ---------------------------------------------------------------------------
// Tab enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum ActiveTab {
    Dashboard,
    P2PNetwork,
}

impl ActiveTab {
    fn next(self) -> Self {
        match self {
            ActiveTab::Dashboard => ActiveTab::P2PNetwork,
            ActiveTab::P2PNetwork => ActiveTab::Dashboard,
        }
    }
}

// ---------------------------------------------------------------------------
// Monitor state
// ---------------------------------------------------------------------------

struct MonitorState {
    stats: Option<StatsData>,
    events: Vec<EventData>, // newest first, max 100
    friends: FriendsData,
    connected: bool,
    active_tab: ActiveTab,
    status_message: Option<String>,
}

impl MonitorState {
    fn new() -> Self {
        Self {
            stats: None,
            events: Vec::new(),
            friends: FriendsData::default(),
            connected: false,
            active_tab: ActiveTab::Dashboard,
            status_message: None,
        }
    }

    fn push_event(&mut self, event: EventData) {
        self.events.insert(0, event);
        self.events.truncate(100);
    }

    /// Returns only the p2p_hit events from the event feed (newest first).
    fn p2p_events(&self) -> Vec<&EventData> {
        self.events
            .iter()
            .filter(|e| e.event_type.to_lowercase() == "p2p_hit")
            .collect()
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
    let friends_url = format!("http://127.0.0.1:{port}/internal/p2p/friends");
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

    // Do an initial stats + friends fetch right away.
    if let Ok(resp) = client.get(&stats_url).send().await {
        if let Ok(data) = resp.json::<StatsData>().await {
            state.stats = Some(data);
            state.connected = true;
        }
    }
    if let Ok(resp) = client.get(&friends_url).send().await {
        if let Ok(data) = resp.json::<FriendsData>().await {
            state.friends = data;
        }
    }

    loop {
        tokio::select! {
            _ = render_interval.tick() => {
                // Check for keyboard events (non-blocking).
                while event::poll(Duration::ZERO)? {
                    if let Event::Key(key) = event::read()? {
                        match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                return Ok(());
                            }
                            KeyCode::Tab | KeyCode::BackTab => {
                                state.active_tab = state.active_tab.next();
                                state.status_message = None;
                            }
                            KeyCode::Char('1') => {
                                state.active_tab = ActiveTab::Dashboard;
                                state.status_message = None;
                            }
                            KeyCode::Char('2') => {
                                state.active_tab = ActiveTab::P2PNetwork;
                                state.status_message = None;
                            }
                            KeyCode::Char('i') if state.active_tab == ActiveTab::P2PNetwork => {
                                // Show the local peer_id as a pseudo invite code.
                                // In the real system this would generate an invite token.
                                state.status_message = Some(
                                    "Invite: share your peer_id from `dja p2p invite`".to_string(),
                                );
                            }
                            _ => {}
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
                // Also refresh the friends list on every stats tick.
                if let Ok(resp) = client.get(&friends_url).send().await {
                    if let Ok(data) = resp.json::<FriendsData>().await {
                        state.friends = data;
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
// Top-level rendering
// ---------------------------------------------------------------------------

fn render(f: &mut Frame, state: &MonitorState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header / status bar
            Constraint::Length(2), // tab bar
            Constraint::Min(3),    // tab content
        ])
        .split(f.area());

    render_header(f, chunks[0], state);
    render_tabs(f, chunks[1], state);

    match state.active_tab {
        ActiveTab::Dashboard => render_dashboard(f, chunks[2], state),
        ActiveTab::P2PNetwork => render_p2p_tab(f, chunks[2], state),
    }
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

    // If there's a status message (e.g. from 'i' key), show it on the right.
    let right_part: Vec<Span> = if let Some(msg) = &state.status_message {
        vec![
            Span::raw("   "),
            Span::styled(msg.as_str(), Style::default().fg(Color::Yellow)),
        ]
    } else {
        vec![
            Span::raw("   "),
            Span::styled(format!("uptime: {uptime_str}"), Style::default().fg(Color::White)),
            Span::raw("   "),
            Span::styled(format!("cache: {cache_str}"), Style::default().fg(Color::Cyan)),
        ]
    };

    let mut spans = vec![
        Span::styled(
            " dja monitor ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        status_dot,
    ];
    spans.extend(right_part);

    let header = Line::from(spans);
    f.render_widget(Paragraph::new(header), area);
}

fn render_tabs(f: &mut Frame, area: Rect, state: &MonitorState) {
    let titles = vec![
        Span::styled(
            " 1 Dashboard ",
            if state.active_tab == ActiveTab::Dashboard {
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
        Span::styled(
            " 2 P2P Network ",
            if state.active_tab == ActiveTab::P2PNetwork {
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ];

    let tab_line = Line::from(
        titles
            .into_iter()
            .flat_map(|s| vec![s, Span::raw("  ")])
            .collect::<Vec<_>>(),
    );

    f.render_widget(Paragraph::new(tab_line), area);
}

// ---------------------------------------------------------------------------
// Dashboard tab (original view)
// ---------------------------------------------------------------------------

fn render_dashboard(f: &mut Frame, area: Rect, state: &MonitorState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // stats panel
            Constraint::Min(3),    // live feed
        ])
        .split(area);

    render_stats(f, chunks[0], state);
    render_live_feed(f, chunks[1], state);
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
            let source_str = ev.source.as_deref().unwrap_or("--").to_string();

            Row::new(vec![
                time_str,
                type_str,
                latency_str,
                snippet_display,
                model_str,
                source_str,
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
        Constraint::Length(16), // source
    ];

    let table = Table::new(rows, widths);
    f.render_widget(table, inner);
}

// ---------------------------------------------------------------------------
// P2P Network tab
// ---------------------------------------------------------------------------

fn render_p2p_tab(f: &mut Frame, area: Rect, state: &MonitorState) {
    // Layout: friends list | pending | p2p stats | activity
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // friends + pending
            Constraint::Length(3), // p2p stats
            Constraint::Min(4),    // activity feed
        ])
        .split(area);

    render_friends_panel(f, chunks[0], state);
    render_p2p_stats(f, chunks[1], state);
    render_p2p_activity(f, chunks[2], state);
}

fn render_friends_panel(f: &mut Frame, area: Rect, state: &MonitorState) {
    let friends = &state.friends.friends;

    let active: Vec<&FriendEntry> = friends
        .iter()
        .filter(|f| f.status == "active")
        .collect();

    let pending: Vec<&FriendEntry> = friends
        .iter()
        .filter(|f| f.status == "pending_sent" || f.status == "pending_received")
        .collect();

    // Split horizontally — active on left, pending on right.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    // --- Active friends ---
    let friends_title = format!(" Friends ({}) ", active.len());
    let friends_block = Block::default()
        .title(Span::styled(
            friends_title,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let friends_inner = friends_block.inner(cols[0]);
    f.render_widget(friends_block, cols[0]);

    if active.is_empty() {
        let msg = Paragraph::new(Span::styled(
            " No active friends yet. Use `dja p2p add`.",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(msg, friends_inner);
    } else {
        let rows: Vec<Row> = active
            .iter()
            .map(|f| {
                Row::new(vec![
                    Span::styled("●", Style::default().fg(Color::Green)).to_string(),
                    f.display_name.clone(),
                    f.public_addr.clone(),
                    "active".to_string(),
                ])
                .style(Style::default().fg(Color::White))
            })
            .collect();

        let widths = [
            Constraint::Length(2),  // bullet
            Constraint::Min(20),    // name
            Constraint::Min(22),    // addr
            Constraint::Length(8),  // status
        ];
        let table = Table::new(rows, widths);
        f.render_widget(table, friends_inner);
    }

    // --- Pending ---
    let pending_block = Block::default()
        .title(Span::styled(
            " Pending ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let pending_inner = pending_block.inner(cols[1]);
    f.render_widget(pending_block, cols[1]);

    if pending.is_empty() {
        let msg = Paragraph::new(Span::styled(
            " No pending invites.",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(msg, pending_inner);
    } else {
        let rows: Vec<Row> = pending
            .iter()
            .map(|f| {
                let (arrow, status_color) = if f.status == "pending_sent" {
                    ("→", Color::Yellow)
                } else {
                    ("←", Color::Cyan)
                };
                Row::new(vec![
                    arrow.to_string(),
                    f.display_name.clone(),
                    format!("({})", f.peer_id.chars().take(8).collect::<String>()),
                    f.status.replace('_', " "),
                ])
                .style(Style::default().fg(status_color))
            })
            .collect();

        let widths = [
            Constraint::Length(2),  // arrow
            Constraint::Min(14),    // name
            Constraint::Length(10), // short peer_id
            Constraint::Min(16),    // status
        ];
        let table = Table::new(rows, widths);
        f.render_widget(table, pending_inner);
    }
}

fn render_p2p_stats(f: &mut Frame, area: Rect, state: &MonitorState) {
    let block = Block::default()
        .title(Span::styled(
            " P2P Stats ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let (hits, served, errors) = match &state.stats {
        Some(s) => (s.p2p_hits, s.p2p_served, s.p2p_errors),
        None => (0, 0, 0),
    };

    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled("Hits: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{hits}"), Style::default().fg(Color::Green)),
        Span::raw("    "),
        Span::styled("Served: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}", format_bytes(served)), Style::default().fg(Color::Cyan)),
        Span::raw("    "),
        Span::styled("Errors: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{errors}"),
            if errors > 0 {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::White)
            },
        ),
    ]);

    f.render_widget(Paragraph::new(line), inner);
}

fn render_p2p_activity(f: &mut Frame, area: Rect, state: &MonitorState) {
    let block = Block::default()
        .title(Span::styled(
            " P2P Activity ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let p2p_events = state.p2p_events();

    if p2p_events.is_empty() {
        let msg = Paragraph::new(Span::styled(
            " No P2P activity yet.",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(msg, inner);
        return;
    }

    let max_rows = inner.height as usize;
    let visible = &p2p_events[..p2p_events.len().min(max_rows)];

    let rows: Vec<Row> = visible
        .iter()
        .map(|ev| {
            let time_str = format_timestamp(&ev.timestamp);
            let snippet = ev
                .prompt_snippet
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(45)
                .collect::<String>();
            let snippet_display = format!("\"{snippet}\"");
            let source_str = ev.source.as_deref().unwrap_or("unknown").to_string();

            Row::new(vec![time_str, "p2p_hit".to_string(), snippet_display, source_str])
                .style(Style::default().fg(Color::Cyan))
        })
        .collect();

    let widths = [
        Constraint::Length(8),  // time
        Constraint::Length(8),  // type
        Constraint::Min(20),   // snippet
        Constraint::Length(18), // source
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
        "p2p_hit" => Color::Cyan,
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

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1}MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1}KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes}B")
    }
}
