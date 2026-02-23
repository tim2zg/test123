//! # Discord-Inspired Terminal UI
//!
//! A three-panel, responsive TUI for Rust4K-P2P modelled after Discord's
//! layout.
//!
//! ```text
//! ┌──────┬─────────────────────┬──────────────────────────────┬──────────────┐
//! │Icons │  Channel List       │  Content Area                │  Members     │
//! │  ◈   │  Rust4K-P2P         │                              │  (wide only) │
//! │      │  ─ STREAMING ─      │                              │              │
//! │      │  # host-stream      │                              │              │
//! │      │  # receive          │                              │              │
//! │      │  ─ CONFIG ─         │                              │              │
//! │      │  ⚙ settings         │                              │              │
//! │      │  ℹ about            │                              │              │
//! │      │  ─────────────────  │                              │              │
//! │      │  ● local · online   │                              │              │
//! └──────┴─────────────────────┴──────────────────────────────┴──────────────┘
//! ```
//!
//! ## Responsive breakpoints
//! | Width    | Panels shown                         |
//! |----------|--------------------------------------|
//! | ≥ 130    | icons + sidebar + content + members  |
//! | 90-129   | icons + sidebar + content            |
//! | 60-89    | sidebar + content                    |
//! | < 60     | content only                         |

use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::signaling::{decode_sdp, encode_sdp, QrPayload};

// ── Discord colour palette ────────────────────────────────────────────────────

const BG: Color = Color::Rgb(49, 51, 56);
const SIDEBAR: Color = Color::Rgb(43, 45, 49);
const DARK: Color = Color::Rgb(30, 31, 34);
const BLURPLE: Color = Color::Rgb(88, 101, 242);
const GREEN: Color = Color::Rgb(35, 165, 90);
const YELLOW: Color = Color::Rgb(240, 178, 50);
const RED: Color = Color::Rgb(242, 63, 67);
const TEXT: Color = Color::Rgb(219, 222, 225);
const MUTED: Color = Color::Rgb(128, 132, 142);
const HEADER_TEXT: Color = Color::Rgb(242, 243, 245);
const INTERACTIVE: Color = Color::Rgb(181, 186, 193);
const INPUT_BG: Color = Color::Rgb(64, 68, 75);
const HOVER_BG: Color = Color::Rgb(53, 55, 60);
const CHANNEL_MUTED: Color = Color::Rgb(147, 151, 160);
const BORDER_COL: Color = Color::Rgb(30, 31, 34);

// ── enums ─────────────────────────────────────────────────────────────────────

/// Which sidebar "channel" is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    HostStream,
    Receive,
    Settings,
    About,
}

impl Channel {
    const ALL: &'static [Channel] = &[
        Channel::HostStream,
        Channel::Receive,
        Channel::Settings,
        Channel::About,
    ];

    fn icon(self) -> &'static str {
        match self {
            Channel::HostStream | Channel::Receive => "#",
            Channel::Settings => "⚙",
            Channel::About => "ℹ",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Channel::HostStream => "host-stream",
            Channel::Receive => "receive",
            Channel::Settings => "settings",
            Channel::About => "about",
        }
    }
}

/// Stream resolution options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Hd720,
    Fhd1080,
    Qhd1440,
    Uhd4K,
    Uhd8K,
}

impl Resolution {
    const ALL: &'static [Resolution] = &[
        Resolution::Hd720,
        Resolution::Fhd1080,
        Resolution::Qhd1440,
        Resolution::Uhd4K,
        Resolution::Uhd8K,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Resolution::Hd720 => "720p  — 1280 × 720",
            Resolution::Fhd1080 => "1080p — 1920 × 1080",
            Resolution::Qhd1440 => "1440p — 2560 × 1440",
            Resolution::Uhd4K => "4K    — 3840 × 2160",
            Resolution::Uhd8K => "8K    — 7680 × 4320",
        }
    }

    pub fn dimensions(self) -> (u32, u32) {
        match self {
            Resolution::Hd720 => (1280, 720),
            Resolution::Fhd1080 => (1920, 1080),
            Resolution::Qhd1440 => (2560, 1440),
            Resolution::Uhd4K => (3840, 2160),
            Resolution::Uhd8K => (7680, 4320),
        }
    }

    pub fn default_bitrate_kbps(self) -> u32 {
        match self {
            Resolution::Hd720 => 5_000,
            Resolution::Fhd1080 => 15_000,
            Resolution::Qhd1440 => 25_000,
            Resolution::Uhd4K => 45_000,
            Resolution::Uhd8K => 100_000,
        }
    }
}

/// Target frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fps {
    Fps30,
    Fps60,
    Fps120,
}

impl Fps {
    const ALL: &'static [Fps] = &[Fps::Fps30, Fps::Fps60, Fps::Fps120];

    fn label(self) -> &'static str {
        match self {
            Fps::Fps30 => "30 fps",
            Fps::Fps60 => "60 fps  (recommended)",
            Fps::Fps120 => "120 fps (high-refresh)",
        }
    }

    pub fn value(self) -> u32 {
        match self {
            Fps::Fps30 => 30,
            Fps::Fps60 => 60,
            Fps::Fps120 => 120,
        }
    }
}

/// Video encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    Auto,
    H264,
    H265,
    Av1,
}

impl Encoder {
    const ALL: &'static [Encoder] = &[Encoder::Auto, Encoder::H264, Encoder::H265, Encoder::Av1];

    fn label(self) -> &'static str {
        match self {
            Encoder::Auto => "Auto  (detect GPU encoder)",
            Encoder::H264 => "H.264 (NVENC / VA-API)",
            Encoder::H265 => "H.265 (NVENC / HEVC)",
            Encoder::Av1 => "AV1   (best quality)",
        }
    }
}

/// All stream settings configurable in the Settings channel.
#[derive(Debug, Clone)]
pub struct StreamSettings {
    pub resolution: Resolution,
    pub fps: Fps,
    pub encoder: Encoder,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            resolution: Resolution::Uhd4K,
            fps: Fps::Fps60,
            encoder: Encoder::Auto,
        }
    }
}

impl StreamSettings {
    /// Returns `(width, height, fps, bitrate_kbps)`.
    pub fn stream_params(&self) -> (u32, u32, u32, u32) {
        let (w, h) = self.resolution.dimensions();
        (w, h, self.fps.value(), self.resolution.default_bitrate_kbps())
    }
}

/// Settings rows that can receive keyboard focus.
#[derive(Debug, Clone, Copy)]
enum SettingsRow {
    Resolution,
    Fps,
    Encoder,
}

impl SettingsRow {
    const ALL: &'static [SettingsRow] =
        &[SettingsRow::Resolution, SettingsRow::Fps, SettingsRow::Encoder];
}

/// Host connection flow state.
#[derive(Debug, Clone, PartialEq)]
pub enum HostStep {
    ShowOffer { payload: String, qr_art: String },
    EnterAnswer { offer_payload: String, qr_art: String, answer_input: String },
    Connecting,
    /// Set by the async WebRTC handshake task once ICE connects.
    #[allow(dead_code)]
    Connected,
}

/// Receiver connection flow state.
#[derive(Debug, Clone, PartialEq)]
pub enum ReceiverStep {
    EnterOffer { input: String },
    ShowAnswer { answer_payload: String, qr_art: String },
    Connecting,
    /// Set by the async WebRTC handshake task once ICE connects.
    #[allow(dead_code)]
    Connected,
}

// ── app state ─────────────────────────────────────────────────────────────────

pub struct App {
    pub channel: Channel,
    pub host_step: Option<HostStep>,
    pub receiver_step: Option<ReceiverStep>,
    pub settings: StreamSettings,
    settings_focus: usize,
    pub status_msg: Option<(String, bool)>,
    pub tick: u64,
    pub quit: bool,
    start_time: Instant,
}

impl App {
    pub fn new() -> Self {
        Self {
            channel: Channel::HostStream,
            host_step: None,
            receiver_step: None,
            settings: StreamSettings::default(),
            settings_focus: 0,
            status_msg: None,
            tick: 0,
            quit: false,
            start_time: Instant::now(),
        }
    }

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    fn spinner(&self) -> char {
        const F: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        F[(self.tick as usize) % F.len()]
    }

    fn elapsed_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    fn make_qr_art(payload: &str) -> String {
        use qrcode::QrCode;
        match QrCode::new(payload.as_bytes()) {
            Ok(code) => code
                .render::<char>()
                .quiet_zone(true)
                .module_dimensions(2, 1)
                .dark_color('█')
                .light_color(' ')
                .build(),
            Err(e) => format!("[QR error: {e}]"),
        }
    }

    fn is_text_input(&self) -> bool {
        matches!(
            (&self.channel, &self.host_step, &self.receiver_step),
            (Channel::HostStream, Some(HostStep::EnterAnswer { .. }), _)
                | (Channel::Receive, _, Some(ReceiverStep::EnterOffer { .. }))
        )
    }

    /// Handle a key press.  Returns `true` when the app should quit.
    pub fn handle_key(&mut self, key: KeyCode) -> bool {
        if key == KeyCode::Char('q') && !self.is_text_input() {
            return true;
        }
        if key == KeyCode::Tab && !self.is_text_input() {
            self.cycle_channel();
            return false;
        }
        match self.channel {
            Channel::HostStream => self.handle_host_key(key),
            Channel::Receive => self.handle_receiver_key(key),
            Channel::Settings => self.handle_settings_key(key),
            Channel::About => {
                if matches!(key, KeyCode::Esc | KeyCode::Char('b')) {
                    self.channel = Channel::HostStream;
                }
            }
        }
        false
    }

    fn cycle_channel(&mut self) {
        let idx = Channel::ALL.iter().position(|&c| c == self.channel).unwrap_or(0);
        self.channel = Channel::ALL[(idx + 1) % Channel::ALL.len()];
    }

    // ── host flow ─────────────────────────────────────────────────────────────

    fn handle_host_key(&mut self, key: KeyCode) {
        match self.host_step.clone() {
            None => {
                if matches!(key, KeyCode::Enter | KeyCode::Char(' ')) {
                    match self.start_host_offer() {
                        Ok(step) => {
                            self.host_step = Some(step);
                            self.status_msg = None;
                        }
                        Err(e) => self.status_msg = Some((format!("Offer error: {e}"), true)),
                    }
                }
            }
            Some(HostStep::ShowOffer { payload, qr_art }) => match key {
                KeyCode::Enter | KeyCode::Char('n') => {
                    self.host_step = Some(HostStep::EnterAnswer {
                        offer_payload: payload,
                        qr_art,
                        answer_input: String::new(),
                    });
                }
                KeyCode::Esc => {
                    self.host_step = None;
                    self.status_msg = None;
                }
                _ => {}
            },
            Some(HostStep::EnterAnswer { offer_payload, qr_art, mut answer_input }) => match key {
                KeyCode::Char(c) => {
                    answer_input.push(c);
                    self.host_step = Some(HostStep::EnterAnswer {
                        offer_payload,
                        qr_art,
                        answer_input,
                    });
                }
                KeyCode::Backspace => {
                    answer_input.pop();
                    self.host_step = Some(HostStep::EnterAnswer {
                        offer_payload,
                        qr_art,
                        answer_input,
                    });
                }
                KeyCode::Enter => {
                    match decode_sdp(&QrPayload(answer_input.trim().to_string())) {
                        Ok(_) => {
                            self.host_step = Some(HostStep::Connecting);
                            self.status_msg =
                                Some(("Answer accepted — establishing P2P link…".into(), false));
                        }
                        Err(e) => {
                            self.status_msg = Some((format!("Invalid answer: {e}"), true));
                            self.host_step = Some(HostStep::EnterAnswer {
                                offer_payload,
                                qr_art,
                                answer_input: String::new(),
                            });
                        }
                    }
                }
                KeyCode::Esc => {
                    self.host_step =
                        Some(HostStep::ShowOffer { payload: offer_payload, qr_art });
                }
                _ => {}
            },
            Some(HostStep::Connecting | HostStep::Connected) => {
                if key == KeyCode::Esc {
                    self.host_step = None;
                    self.status_msg = None;
                }
            }
        }
    }

    fn start_host_offer(&self) -> Result<HostStep> {
        let (w, h, fps, bitrate_kbps) = self.settings.stream_params();
        // max-fs: maximum frame size in H.264 macroblocks (16×16 px blocks).
        let max_fs = (w.div_ceil(16)) * (h.div_ceil(16));
        let demo_sdp = format!(
            "v=0\no=- 0 0 IN IP4 127.0.0.1\ns=-\nt=0 0\n\
             m=video 9 UDP/TLS/RTP/SAVPF 96\nb=AS:{bitrate_kbps}\n\
             a=ice-ufrag:demo\na=ice-pwd:demopassword00000000001\n\
             a=sendonly\na=rtpmap:96 H264/90000\n\
             a=fmtp:96 max-fs={max_fs};max-fr={fps}\n"
        );
        let qr_payload = encode_sdp(&demo_sdp)?;
        let qr_art = Self::make_qr_art(qr_payload.as_str());
        Ok(HostStep::ShowOffer { payload: qr_payload.0, qr_art })
    }

    // ── receiver flow ─────────────────────────────────────────────────────────

    fn handle_receiver_key(&mut self, key: KeyCode) {
        match self.receiver_step.clone() {
            None => {
                if matches!(key, KeyCode::Enter | KeyCode::Char(' ')) {
                    self.receiver_step =
                        Some(ReceiverStep::EnterOffer { input: String::new() });
                    self.status_msg = None;
                }
            }
            Some(ReceiverStep::EnterOffer { mut input }) => match key {
                KeyCode::Char(c) => {
                    input.push(c);
                    self.receiver_step = Some(ReceiverStep::EnterOffer { input });
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.receiver_step = Some(ReceiverStep::EnterOffer { input });
                }
                KeyCode::Enter => {
                    let trimmed = input.trim().to_string();
                    match self.build_answer_qr(&trimmed) {
                        Ok((payload, qr_art)) => {
                            self.receiver_step = Some(ReceiverStep::ShowAnswer {
                                answer_payload: payload,
                                qr_art,
                            });
                            self.status_msg = None;
                        }
                        Err(e) => {
                            self.status_msg = Some((format!("Invalid offer: {e}"), true));
                            self.receiver_step =
                                Some(ReceiverStep::EnterOffer { input: String::new() });
                        }
                    }
                }
                KeyCode::Esc => {
                    self.receiver_step = None;
                    self.status_msg = None;
                }
                _ => {}
            },
            Some(ReceiverStep::ShowAnswer { .. }) => match key {
                KeyCode::Enter => {
                    self.receiver_step = Some(ReceiverStep::Connecting);
                    self.status_msg =
                        Some(("Waiting for host to complete handshake…".into(), false));
                }
                KeyCode::Esc => {
                    self.receiver_step = None;
                    self.status_msg = None;
                }
                _ => {}
            },
            Some(ReceiverStep::Connecting | ReceiverStep::Connected) => {
                if key == KeyCode::Esc {
                    self.receiver_step = None;
                    self.status_msg = None;
                }
            }
        }
    }

    fn build_answer_qr(&self, offer_payload: &str) -> Result<(String, String)> {
        let offer_sdp = decode_sdp(&QrPayload(offer_payload.to_string()))?;
        let answer_sdp = offer_sdp.replace("a=sendonly", "a=recvonly");
        let qr_payload = encode_sdp(&answer_sdp)?;
        let qr_art = Self::make_qr_art(qr_payload.as_str());
        Ok((qr_payload.0, qr_art))
    }

    // ── settings ──────────────────────────────────────────────────────────────

    fn handle_settings_key(&mut self, key: KeyCode) {
        let n = SettingsRow::ALL.len();
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                self.settings_focus = self.settings_focus.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.settings_focus + 1 < n {
                    self.settings_focus += 1;
                }
            }
            KeyCode::Left | KeyCode::Char('h') => self.settings_step(false),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter | KeyCode::Char(' ') => {
                self.settings_step(true)
            }
            _ => {}
        }
    }

    fn settings_step(&mut self, forward: bool) {
        match SettingsRow::ALL[self.settings_focus] {
            SettingsRow::Resolution => {
                let i = Resolution::ALL
                    .iter()
                    .position(|&r| r == self.settings.resolution)
                    .unwrap_or(0);
                self.settings.resolution =
                    Resolution::ALL[cycle_index(i, Resolution::ALL.len(), forward)];
                self.status_msg = Some((
                    format!("Resolution → {}", self.settings.resolution.label()),
                    false,
                ));
            }
            SettingsRow::Fps => {
                let i = Fps::ALL.iter().position(|&f| f == self.settings.fps).unwrap_or(0);
                self.settings.fps = Fps::ALL[cycle_index(i, Fps::ALL.len(), forward)];
                self.status_msg =
                    Some((format!("FPS → {}", self.settings.fps.label()), false));
            }
            SettingsRow::Encoder => {
                let i = Encoder::ALL
                    .iter()
                    .position(|&e| e == self.settings.encoder)
                    .unwrap_or(0);
                self.settings.encoder =
                    Encoder::ALL[cycle_index(i, Encoder::ALL.len(), forward)];
                self.status_msg = Some((
                    format!("Encoder → {}", self.settings.encoder.label()),
                    false,
                ));
            }
        }
    }
}

// ── responsive layout ─────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum LayoutMode {
    Full,    // icons + sidebar + content + members  (≥ 130 cols)
    Compact, // icons + sidebar + content            (90-129 cols)
    Minimal, // sidebar + content                    (60-89 cols)
    Single,  // content only                         (< 60 cols)
}

fn layout_mode(w: u16) -> LayoutMode {
    if w >= 130 {
        LayoutMode::Full
    } else if w >= 90 {
        LayoutMode::Compact
    } else if w >= 60 {
        LayoutMode::Minimal
    } else {
        LayoutMode::Single
    }
}

// ── entry point ───────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let tick_rate = Duration::from_millis(80);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| draw(f, &app))?;

        let timeout = tick_rate.checked_sub(last_tick.elapsed()).unwrap_or_default();
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && app.handle_key(key.code) {
                    app.quit = true;
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
        }

        if app.quit {
            break;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

// ── top-level draw ────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App) {
    f.render_widget(Block::default().style(Style::default().bg(BG)), f.area());

    match layout_mode(f.area().width) {
        LayoutMode::Full => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(6),
                    Constraint::Length(22),
                    Constraint::Min(0),
                    Constraint::Length(20),
                ])
                .split(f.area());
            draw_icon_strip(f, cols[0]);
            draw_sidebar(f, cols[1], app);
            draw_content_area(f, cols[2], app);
            draw_members_panel(f, cols[3], app);
        }
        LayoutMode::Compact => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(6),
                    Constraint::Length(22),
                    Constraint::Min(0),
                ])
                .split(f.area());
            draw_icon_strip(f, cols[0]);
            draw_sidebar(f, cols[1], app);
            draw_content_area(f, cols[2], app);
        }
        LayoutMode::Minimal => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(20), Constraint::Min(0)])
                .split(f.area());
            draw_sidebar(f, cols[0], app);
            draw_content_area(f, cols[1], app);
        }
        LayoutMode::Single => draw_content_area(f, f.area(), app),
    }
}

// ── server icon strip ─────────────────────────────────────────────────────────

fn draw_icon_strip(f: &mut Frame, area: Rect) {
    f.render_widget(Block::default().style(Style::default().bg(DARK)), area);

    let icon_area = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: 3.min(area.height),
    };

    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("╭──╮", Style::default().fg(BLURPLE))),
            Line::from(vec![
                Span::styled("│", Style::default().fg(BLURPLE)),
                Span::styled("4K", Style::default().fg(HEADER_TEXT).add_modifier(Modifier::BOLD)),
                Span::styled("│", Style::default().fg(BLURPLE)),
            ]),
            Line::from(Span::styled("╰──╯", Style::default().fg(BLURPLE))),
        ])
        .alignment(Alignment::Center),
        icon_area,
    );
}

// ── channel sidebar ───────────────────────────────────────────────────────────

fn draw_sidebar(f: &mut Frame, area: Rect, app: &App) {
    f.render_widget(Block::default().style(Style::default().bg(SIDEBAR)), area);

    // Title bar
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                " Rust4K-P2P",
                Style::default().fg(HEADER_TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                " ─────────────────",
                Style::default().fg(BORDER_COL),
            )),
        ]),
        Rect { x: area.x, y: area.y, width: area.width, height: 2 },
    );

    // Channel list
    let body = Rect {
        x: area.x,
        y: area.y + 2,
        width: area.width,
        height: area.height.saturating_sub(5),
    };

    let mut items: Vec<ListItem> = Vec::new();
    items.push(sidebar_section("STREAMING"));
    for &ch in &[Channel::HostStream, Channel::Receive] {
        items.push(sidebar_channel(ch, ch == app.channel));
    }
    items.push(sidebar_section("CONFIG"));
    for &ch in &[Channel::Settings, Channel::About] {
        items.push(sidebar_channel(ch, ch == app.channel));
    }

    f.render_widget(List::new(items).style(Style::default().bg(SIDEBAR)), body);

    // User bar
    if area.height >= 5 {
        let user_area = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(3),
            width: area.width,
            height: 3,
        };
        draw_user_bar(f, user_area);
    }
}

fn sidebar_section(title: &str) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(title.to_string(), Style::default().fg(MUTED).add_modifier(Modifier::BOLD)),
    ]))
}

fn sidebar_channel(ch: Channel, active: bool) -> ListItem<'static> {
    let (bg, text_col) = if active {
        (HOVER_BG, HEADER_TEXT)
    } else {
        (SIDEBAR, CHANNEL_MUTED)
    };
    let prefix = if active { "▌ " } else { "  " };
    ListItem::new(Line::from(vec![
        Span::styled(prefix.to_string(), Style::default().fg(BLURPLE)),
        Span::styled(
            ch.icon().to_string(),
            Style::default().fg(if active { BLURPLE } else { MUTED }),
        ),
        Span::raw(" "),
        Span::styled(ch.name().to_string(), Style::default().fg(text_col)),
    ]))
    .style(Style::default().bg(bg))
}

fn draw_user_bar(f: &mut Frame, area: Rect) {
    f.render_widget(Block::default().style(Style::default().bg(DARK)), area);
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("● ", Style::default().fg(GREEN)),
            Span::styled("local · online", Style::default().fg(INTERACTIVE)),
        ])),
        inner,
    );
}

// ── members panel ─────────────────────────────────────────────────────────────

fn draw_members_panel(f: &mut Frame, area: Rect, app: &App) {
    f.render_widget(Block::default().style(Style::default().bg(SIDEBAR)), area);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                " Members",
                Style::default().fg(HEADER_TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(" ─────────────────", Style::default().fg(BORDER_COL))),
        ]),
        Rect { x: area.x, y: area.y, width: area.width, height: 2 },
    );

    let body = Rect {
        x: area.x,
        y: area.y + 3,
        width: area.width,
        height: area.height.saturating_sub(3),
    };

    let streaming = matches!(&app.host_step, Some(HostStep::Connecting | HostStep::Connected));

    let mut lines = vec![
        Line::from(Span::styled(
            " ONLINE — 1",
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(" ● ", Style::default().fg(GREEN)),
            Span::styled("You", Style::default().fg(TEXT)),
        ]),
    ];
    if streaming {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("◉ LIVE", Style::default().fg(RED).add_modifier(Modifier::BOLD)),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " OFFLINE — 0",
        Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
    )));

    f.render_widget(Paragraph::new(lines), body);
}

// ── content area ──────────────────────────────────────────────────────────────

fn draw_content_area(f: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(2)])
        .split(area);

    draw_content_titlebar(f, rows[0], app);

    match app.channel {
        Channel::HostStream => draw_host_channel(f, rows[1], app),
        Channel::Receive => draw_receiver_channel(f, rows[1], app),
        Channel::Settings => draw_settings_channel(f, rows[1], app),
        Channel::About => draw_about_channel(f, rows[1]),
    }

    draw_statusbar(f, rows[2], app);
}

fn draw_content_titlebar(f: &mut Frame, area: Rect, app: &App) {
    f.render_widget(Block::default().style(Style::default().bg(BG)), area);

    // Bottom rule
    let sep_y = area.y + area.height.saturating_sub(1);
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(BORDER_COL),
        )),
        Rect { x: area.x, y: sep_y, width: area.width, height: 1 },
    );

    let (w, h, fps, _) = app.settings.stream_params();
    let ch = app.channel;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", ch.icon()),
                Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                ch.name(),
                Style::default().fg(HEADER_TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  │  ", Style::default().fg(BORDER_COL)),
            Span::styled(format!("{w}×{h} @ {fps} fps"), Style::default().fg(MUTED)),
            Span::styled("  │  Tab: switch  q: quit", Style::default().fg(MUTED)),
        ])),
        Rect { x: area.x, y: area.y, width: area.width, height: 1 },
    );
}

fn draw_statusbar(f: &mut Frame, area: Rect, app: &App) {
    f.render_widget(Block::default().style(Style::default().bg(DARK)), area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    let (left, right) = if let Some((msg, is_err)) = &app.status_msg {
        let col = if *is_err { RED } else { GREEN };
        let icon = if *is_err { "✖ " } else { "✔ " };
        (
            Line::from(vec![
                Span::styled(icon, Style::default().fg(col).add_modifier(Modifier::BOLD)),
                Span::styled(msg.as_str(), Style::default().fg(col)),
            ]),
            Line::from(Span::styled("Rust4K-P2P v0.1.0", Style::default().fg(MUTED))),
        )
    } else {
        (
            Line::from(Span::styled("Ready", Style::default().fg(MUTED))),
            Line::from(Span::styled("Rust4K-P2P v0.1.0", Style::default().fg(MUTED))),
        )
    };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(20)])
        .split(inner);

    f.render_widget(Paragraph::new(left), cols[0]);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), cols[1]);
}

// ── host-stream channel ───────────────────────────────────────────────────────

fn draw_host_channel(f: &mut Frame, area: Rect, app: &App) {
    match &app.host_step {
        None => draw_host_welcome(f, area, app),
        Some(HostStep::ShowOffer { payload, qr_art }) => {
            draw_host_offer(f, area, payload, qr_art)
        }
        Some(HostStep::EnterAnswer { qr_art, answer_input, .. }) => {
            draw_host_enter_answer(f, area, qr_art, answer_input)
        }
        Some(HostStep::Connecting) => draw_host_connecting(f, area, app),
        Some(HostStep::Connected) => draw_host_connected(f, area, app),
    }
}

fn draw_host_welcome(f: &mut Frame, area: Rect, app: &App) {
    let (w, h, fps, kbps) = app.settings.stream_params();

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(area);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "  📡  Host / Sender Mode",
                Style::default().fg(HEADER_TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from(Span::styled(
                "  Stream your desktop to a peer — no server required.",
                Style::default().fg(TEXT),
            )),
            Line::default(),
            Line::from(vec![
                Span::styled("  Resolution  ", Style::default().fg(MUTED)),
                Span::styled(
                    format!("{w}×{h} @ {fps} fps"),
                    Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Bitrate     ", Style::default().fg(MUTED)),
                Span::styled(
                    format!("{} Mbps", kbps / 1000),
                    Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Encoder     ", Style::default().fg(MUTED)),
                Span::styled(
                    app.settings.encoder.label(),
                    Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Transport   ", Style::default().fg(MUTED)),
                Span::styled(
                    "UDP (STUN hole-punch)",
                    Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::default(),
            Line::from(Span::styled(
                "  Change resolution & encoder in ⚙ settings  (Tab to switch)",
                Style::default().fg(MUTED),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BLURPLE))
                .style(Style::default().bg(SIDEBAR))
                .padding(Padding::uniform(1)),
        ),
        rows[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  [ ", Style::default().fg(MUTED)),
            Span::styled("Enter", Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD)),
            Span::styled(" ]  Generate offer QR & start streaming setup", Style::default().fg(TEXT)),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER_COL))
                .style(Style::default().bg(DARK)),
        ),
        rows[1],
    );
}

fn draw_host_offer(f: &mut Frame, area: Rect, payload: &str, qr_art: &str) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);

    draw_stepper(f, rows[0], 0, &["Offer QR", "Enter Answer", "Connecting", "Streaming"]);

    let is_wide = area.width >= 80;
    if is_wide {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .margin(1)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(rows[1]);

        // QR panel (with payload strip at the bottom)
        let qr_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(4)])
            .split(cols[0]);

        f.render_widget(
            Paragraph::new(qr_art)
                .block(discord_block(" ◈ Offer QR Code ", BLURPLE))
                .style(Style::default().fg(TEXT).bg(SIDEBAR)),
            qr_rows[0],
        );
        f.render_widget(
            Paragraph::new(payload)
                .block(discord_block(" Full Payload ", MUTED))
                .style(Style::default().fg(YELLOW))
                .wrap(Wrap { trim: true }),
            qr_rows[1],
        );

        // Instructions
        let trunc = truncate(payload, 54);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("  Share offer with receiver", Style::default().fg(HEADER_TEXT).add_modifier(Modifier::BOLD))),
                Line::default(),
                discord_line("Have the receiver scan this QR or paste the payload.", TEXT),
                Line::default(),
                Line::from(Span::styled("  Payload preview:", Style::default().fg(MUTED))),
                Line::from(Span::styled(format!("  {trunc}"), Style::default().fg(YELLOW))),
                Line::default(),
                discord_line("→ Enter  advance to enter answer", GREEN),
                discord_line("→ Esc    cancel & reset", MUTED),
            ])
            .block(discord_block(" Instructions ", MUTED))
            .wrap(Wrap { trim: false }),
            cols[1],
        );
    } else {
        let margin = Rect {
            x: rows[1].x + 1,
            y: rows[1].y,
            width: rows[1].width.saturating_sub(2),
            height: rows[1].height,
        };
        f.render_widget(
            Paragraph::new(qr_art)
                .block(discord_block(" ◈ Offer QR Code ", BLURPLE))
                .style(Style::default().fg(TEXT).bg(SIDEBAR)),
            margin,
        );
    }
}

fn draw_host_enter_answer(f: &mut Frame, area: Rect, qr_art: &str, input: &str) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0), Constraint::Length(4)])
        .split(area);

    draw_stepper(f, rows[0], 1, &["Offer QR", "Enter Answer", "Connecting", "Streaming"]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .margin(1)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);

    f.render_widget(
        Paragraph::new(qr_art)
            .block(discord_block(" Your Offer QR (sent) ", MUTED))
            .style(Style::default().fg(MUTED).bg(SIDEBAR)),
        cols[0],
    );

    f.render_widget(
        Paragraph::new(vec![
            discord_line("Scan the receiver's answer QR and paste below.", TEXT),
            Line::default(),
            discord_line("Payload starts with 'H4sI…'", MUTED),
            Line::default(),
            discord_line("→ Enter  confirm", GREEN),
            discord_line("→ Esc    back to QR", MUTED),
        ])
        .block(discord_block(" Instructions ", MUTED))
        .wrap(Wrap { trim: false }),
        cols[1],
    );

    let cursor = if blink() { '▌' } else { ' ' };
    f.render_widget(
        Paragraph::new(format!("{input}{cursor}"))
            .block(discord_block(" Paste Receiver's Answer Payload ", BLURPLE))
            .style(Style::default().fg(TEXT).bg(INPUT_BG)),
        rows[2],
    );
}

fn draw_host_connecting(f: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);
    draw_stepper(f, rows[0], 2, &["Offer QR", "Enter Answer", "Connecting", "Streaming"]);
    draw_connecting_widget(f, rows[1], app, "Establishing P2P link with receiver…");
}

fn draw_host_connected(f: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);
    draw_stepper(f, rows[0], 3, &["Offer QR", "Enter Answer", "Connecting", "Streaming"]);
    let (w, h, fps, kbps) = app.settings.stream_params();
    draw_connected_widget(f, rows[1], &format!("{w}×{h} @ {fps} fps · {} Mbps", kbps / 1000));
}

// ── receive channel ───────────────────────────────────────────────────────────

fn draw_receiver_channel(f: &mut Frame, area: Rect, app: &App) {
    match &app.receiver_step {
        None => draw_receiver_welcome(f, area),
        Some(ReceiverStep::EnterOffer { input }) => draw_receiver_enter_offer(f, area, input),
        Some(ReceiverStep::ShowAnswer { answer_payload, qr_art }) => {
            draw_receiver_answer(f, area, answer_payload, qr_art)
        }
        Some(ReceiverStep::Connecting) => draw_receiver_connecting(f, area, app),
        Some(ReceiverStep::Connected) => draw_receiver_connected(f, area, app),
    }
}

fn draw_receiver_welcome(f: &mut Frame, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(area);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "  📺  Receiver / Client Mode",
                Style::default().fg(HEADER_TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            discord_line("Receive a 4K desktop stream directly from a peer.", TEXT),
            Line::default(),
            discord_line("The host will display a QR code — scan it or copy the text payload.", TEXT),
            Line::default(),
            Line::from(Span::styled("  Requirements:", Style::default().fg(MUTED).add_modifier(Modifier::BOLD))),
            discord_line("• 4K monitor for native scaling", TEXT),
            discord_line("• Hardware decode: H.264 / H.265 / AV1", TEXT),
            discord_line("• UDP traffic allowed (STUN hole-punch)", TEXT),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(GREEN))
                .style(Style::default().bg(SIDEBAR))
                .padding(Padding::uniform(1)),
        ),
        rows[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  [ ", Style::default().fg(MUTED)),
            Span::styled("Enter", Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD)),
            Span::styled(" ]  Start receiving — paste offer payload", Style::default().fg(TEXT)),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER_COL))
                .style(Style::default().bg(DARK)),
        ),
        rows[1],
    );
}

fn draw_receiver_enter_offer(f: &mut Frame, area: Rect, input: &str) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0), Constraint::Length(4)])
        .split(area);

    draw_stepper(f, rows[0], 0, &["Enter Offer", "Answer QR", "Connecting", "Receiving"]);

    f.render_widget(
        Paragraph::new(vec![
            discord_line("Scan the host's QR, then paste the payload below.", TEXT),
            Line::default(),
            discord_line("The payload is a Base64+Gzip SDP string starting with 'H4sI…'.", MUTED),
            Line::default(),
            discord_line("→ Enter  decode & generate answer QR", GREEN),
            discord_line("→ Esc    cancel", MUTED),
        ])
        .block(discord_block(" Enter Host's Offer ", GREEN))
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(SIDEBAR)),
        rows[1],
    );

    let cursor = if blink() { '▌' } else { ' ' };
    f.render_widget(
        Paragraph::new(format!("{input}{cursor}"))
            .block(discord_block(" Paste Offer Payload ", BLURPLE))
            .style(Style::default().fg(YELLOW).bg(INPUT_BG)),
        rows[2],
    );
}

fn draw_receiver_answer(f: &mut Frame, area: Rect, payload: &str, qr_art: &str) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);

    draw_stepper(f, rows[0], 1, &["Enter Offer", "Answer QR", "Connecting", "Receiving"]);

    let is_wide = area.width >= 80;
    if is_wide {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .margin(1)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(rows[1]);

        let qr_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(4)])
            .split(cols[0]);

        f.render_widget(
            Paragraph::new(qr_art)
                .block(discord_block(" ◈ Answer QR Code ", GREEN))
                .style(Style::default().fg(TEXT).bg(SIDEBAR)),
            qr_rows[0],
        );
        f.render_widget(
            Paragraph::new(payload)
                .block(discord_block(" Full Payload ", MUTED))
                .style(Style::default().fg(YELLOW))
                .wrap(Wrap { trim: true }),
            qr_rows[1],
        );

        let trunc = truncate(payload, 54);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("  Share answer with host", Style::default().fg(HEADER_TEXT).add_modifier(Modifier::BOLD))),
                Line::default(),
                discord_line("Have the host scan this QR or paste the payload.", TEXT),
                Line::default(),
                Line::from(Span::styled("  Payload preview:", Style::default().fg(MUTED))),
                Line::from(Span::styled(format!("  {trunc}"), Style::default().fg(YELLOW))),
                Line::default(),
                discord_line("→ Enter  wait for connection", GREEN),
                discord_line("→ Esc    start over", MUTED),
            ])
            .block(discord_block(" Instructions ", MUTED))
            .wrap(Wrap { trim: false }),
            cols[1],
        );
    } else {
        let margin = Rect {
            x: rows[1].x + 1,
            y: rows[1].y,
            width: rows[1].width.saturating_sub(2),
            height: rows[1].height,
        };
        f.render_widget(
            Paragraph::new(qr_art)
                .block(discord_block(" ◈ Answer QR Code ", GREEN))
                .style(Style::default().fg(TEXT).bg(SIDEBAR)),
            margin,
        );
    }
}

fn draw_receiver_connecting(f: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);
    draw_stepper(f, rows[0], 2, &["Enter Offer", "Answer QR", "Connecting", "Receiving"]);
    draw_connecting_widget(f, rows[1], app, "Waiting for host to complete handshake…");
}

fn draw_receiver_connected(f: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);
    draw_stepper(f, rows[0], 3, &["Enter Offer", "Answer QR", "Connecting", "Receiving"]);
    let (w, h, fps, kbps) = app.settings.stream_params();
    draw_connected_widget(f, rows[1], &format!("Receiving {w}×{h} @ {fps} fps · {} Mbps", kbps / 1000));
}

// ── settings channel ──────────────────────────────────────────────────────────

fn draw_settings_channel(f: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Length(3), Constraint::Length(10), Constraint::Min(0)])
        .split(area);

    // Section header
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "  ⚙  Stream Settings",
                Style::default().fg(HEADER_TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "  ↑↓ / j k   select row       ←→ / h l  or  Enter  cycle value",
                Style::default().fg(MUTED),
            )),
        ]),
        rows[0],
    );

    // Settings table
    let focus = app.settings_focus;
    let setting_rows: &[(&str, &str, bool)] = &[
        ("  Resolution ", app.settings.resolution.label(), focus == 0),
        ("  Frame rate ", app.settings.fps.label(), focus == 1),
        ("  Encoder    ", app.settings.encoder.label(), focus == 2),
    ];

    let items: Vec<ListItem> = setting_rows
        .iter()
        .map(|(label, value, focused)| {
            let (bg, fg_label, fg_val) = if *focused {
                (HOVER_BG, HEADER_TEXT, BLURPLE)
            } else {
                (SIDEBAR, INTERACTIVE, TEXT)
            };
            let prefix = if *focused { " ▶ " } else { "   " };
            let bold = if *focused { Modifier::BOLD } else { Modifier::empty() };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(prefix, Style::default().fg(BLURPLE)),
                    Span::styled(label.to_string(), Style::default().fg(fg_label).add_modifier(bold)),
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        format!("[ {value} ]"),
                        Style::default().fg(fg_val).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::default(),
            ])
            .style(Style::default().bg(bg))
        })
        .collect();

    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BLURPLE))
                .style(Style::default().bg(SIDEBAR))
                .padding(Padding::horizontal(1)),
        ),
        rows[1],
    );

    // Resolution picker + stream preview
    let (w, h, fps, kbps) = app.settings.stream_params();
    let mbps = kbps / 1000;

    let mut preview: Vec<Line> = vec![
        Line::from(Span::styled(
            "  Stream Preview",
            Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
    ];

    for &r in Resolution::ALL {
        let selected = r == app.settings.resolution;
        let (rw, rh) = r.dimensions();
        let rb = r.default_bitrate_kbps() / 1000;
        let mark = if selected { "●" } else { "○" };
        let col = if selected { BLURPLE } else { MUTED };
        let name_col = if selected { HEADER_TEXT } else { TEXT };
        preview.push(Line::from(vec![
            Span::styled(format!("  {mark} "), Style::default().fg(col)),
            Span::styled(format!("{rw}×{rh}"), Style::default().fg(name_col)),
            Span::styled(format!("  (~{rb} Mbps)"), Style::default().fg(MUTED)),
        ]));
    }

    preview.push(Line::default());
    preview.push(Line::from(vec![
        Span::styled("  Active:   ", Style::default().fg(MUTED)),
        Span::styled(
            format!("{w}×{h} @ {fps} fps  ·  {mbps} Mbps"),
            Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
        ),
    ]));
    preview.push(Line::from(vec![
        Span::styled("  Upload:   ", Style::default().fg(MUTED)),
        Span::styled(
            format!(">{} Mbps recommended", mbps + 5),
            Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
        ),
    ]));

    f.render_widget(
        Paragraph::new(preview).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER_COL))
                .style(Style::default().bg(SIDEBAR))
                .padding(Padding::horizontal(1)),
        ),
        rows[2],
    );
}

// ── about channel ─────────────────────────────────────────────────────────────

fn draw_about_channel(f: &mut Frame, area: Rect) {
    if area.width >= 80 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .margin(2)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        f.render_widget(about_left_panel(), cols[0]);
        f.render_widget(about_right_panel(), cols[1]);
    } else {
        let body = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        f.render_widget(about_left_panel(), body);
    }
}

fn about_left_panel<'a>() -> Paragraph<'a> {
    Paragraph::new(vec![
        Line::from(Span::styled(
            "Rust4K-P2P",
            Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled("Decentralized 4K desktop streaming", Style::default().fg(TEXT))),
        Line::default(),
        Line::from(Span::styled("Tech Stack", Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD))),
        discord_line("Language  Rust (memory-safe, zero-copy)", TEXT),
        discord_line("WebRTC    webrtc-rs (P2P, STUN/ICE)", TEXT),
        discord_line("Media     GStreamer (NVENC/AV1/x264)", TEXT),
        discord_line("Audio     CPAL / Oboe (Opus codec)", TEXT),
        discord_line("UI        ratatui + crossterm", TEXT),
        Line::default(),
        Line::from(Span::styled("Signaling", Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD))),
        discord_line("SDP → Gzip → Base64 → QR code", TEXT),
        discord_line("No server required (airgap QR exchange)", GREEN),
        discord_line("UDP hole-punch via STUN", TEXT),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER_COL))
            .title(Span::styled(
                " About ",
                Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(SIDEBAR))
            .padding(Padding::uniform(1)),
    )
    .wrap(Wrap { trim: false })
}

fn about_right_panel<'a>() -> Paragraph<'a> {
    Paragraph::new(vec![
        Line::from(Span::styled("Keybindings", Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD))),
        Line::default(),
        kb("Tab", "Cycle channels"),
        kb("↑↓/jk", "Navigate"),
        kb("←→/hl", "Change setting value"),
        kb("Enter", "Confirm / advance"),
        kb("Esc", "Go back / cancel"),
        kb("q", "Quit (outside input)"),
        Line::default(),
        Line::from(Span::styled("Requirements", Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from(Span::styled("  Host (Sender)", Style::default().fg(YELLOW).add_modifier(Modifier::BOLD))),
        discord_line("• Upload > 50 Mbps (4K) / 15 Mbps (1080p)", TEXT),
        discord_line("• NVIDIA / AMD / Apple Silicon GPU", TEXT),
        Line::default(),
        Line::from(Span::styled("  Receiver", Style::default().fg(YELLOW).add_modifier(Modifier::BOLD))),
        discord_line("• 4K monitor for native scaling", TEXT),
        discord_line("• HW decode: VP9 / AV1 / H.265", TEXT),
        Line::default(),
        Line::from(Span::styled("  Build", Style::default().fg(YELLOW).add_modifier(Modifier::BOLD))),
        discord_line("--features gstreamer  (HW capture)", MUTED),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER_COL))
            .title(Span::styled(
                " Help ",
                Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(SIDEBAR))
            .padding(Padding::uniform(1)),
    )
    .wrap(Wrap { trim: false })
}

// ── shared widgets ────────────────────────────────────────────────────────────

fn draw_stepper(f: &mut Frame, area: Rect, active: usize, labels: &[&str]) {
    let n = labels.len() as u32;
    let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Ratio(1, n)).collect();
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (i, (&col, label)) in cols.iter().zip(labels.iter()).enumerate() {
        let (sty, prefix) = if i < active {
            (Style::default().fg(GREEN), "✓ ")
        } else if i == active {
            (Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD), "▶ ")
        } else {
            (Style::default().fg(MUTED), "  ")
        };
        let border_sty = if i == active {
            Style::default().fg(BLURPLE)
        } else {
            Style::default().fg(BORDER_COL)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, sty),
                Span::styled(format!("{}. {label}", i + 1), sty),
            ]))
            .block(Block::default().borders(Borders::BOTTOM).border_style(border_sty))
            .alignment(Alignment::Center),
            col,
        );
    }
}

fn draw_connecting_widget(f: &mut Frame, area: Rect, app: &App, msg: &str) {
    let center = center_rect(62, 12, area);
    f.render_widget(Clear, center);
    f.render_widget(
        Paragraph::new(vec![
            Line::default(),
            Line::from(Span::styled(
                format!("  {}  {msg}", app.spinner()),
                Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from(Span::styled(
                format!("  Elapsed: {}s  —  UDP hole-punch via STUN", app.elapsed_secs()),
                Style::default().fg(MUTED),
            )),
            Line::default(),
            Line::from(Span::styled(
                "  Ensure UDP traffic is allowed on your network.",
                Style::default().fg(YELLOW),
            )),
            Line::default(),
            discord_line("Esc  cancel", MUTED),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BLURPLE))
                .title(Span::styled(
                    " Connecting… ",
                    Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD),
                ))
                .title_alignment(Alignment::Center)
                .style(Style::default().bg(SIDEBAR)),
        )
        .alignment(Alignment::Left),
        center,
    );
}

fn draw_connected_widget(f: &mut Frame, area: Rect, detail: &str) {
    let center = center_rect(62, 12, area);
    f.render_widget(Clear, center);
    f.render_widget(
        Paragraph::new(vec![
            Line::default(),
            Line::from(Span::styled(
                "  ✓ P2P link established!",
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from(Span::styled(format!("  {detail}"), Style::default().fg(TEXT))),
            Line::default(),
            Line::from(vec![
                Span::styled("  Transport  ", Style::default().fg(MUTED)),
                Span::styled("UDP (STUN hole-punch)", Style::default().fg(GREEN)),
            ]),
            Line::from(vec![
                Span::styled("  Codec      ", Style::default().fg(MUTED)),
                Span::styled("H.264 / H.265 / AV1", Style::default().fg(GREEN)),
            ]),
            Line::default(),
            discord_line("Esc  disconnect & return", MUTED),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(GREEN))
                .title(Span::styled(
                    " ✓ Connected ",
                    Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                ))
                .title_alignment(Alignment::Center)
                .style(Style::default().bg(SIDEBAR)),
        ),
        center,
    );
}

// ── tiny helpers ──────────────────────────────────────────────────────────────

fn discord_block(title: &str, border_col: Color) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_col))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(border_col).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(SIDEBAR))
        .padding(Padding::uniform(1))
}

fn discord_line(text: &str, col: Color) -> Line<'_> {
    Line::from(Span::styled(format!("  {text}"), Style::default().fg(col)))
}

fn kb(key: &'static str, desc: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {key:<8}"), Style::default().fg(BLURPLE).add_modifier(Modifier::BOLD)),
        Span::styled(desc, Style::default().fg(TEXT)),
    ])
}

fn blink() -> bool {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis()
        < 500
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}…", &s[..max])
    } else {
        s.to_string()
    }
}

fn center_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// Cycle an index forward or backward within `[0, len)`, wrapping around.
fn cycle_index(current: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    }
}
