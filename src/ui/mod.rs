//! # Terminal User Interface
//!
//! A rich multi-screen TUI for Rust4K-P2P built with [`ratatui`] and
//! [`crossterm`].
//!
//! ## Screens
//! * **Home** — splash / mode selection  
//! * **Host** — guided host (offer → QR → wait for answer → connected)  
//! * **Receiver** — guided receiver (paste offer → answer QR → connected)  
//! * **About** — project information & keybindings  

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
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap,
    },
    Frame, Terminal,
};

use crate::signaling::{decode_sdp, encode_sdp, QrPayload};

// ── palette ───────────────────────────────────────────────────────────────────

const ACCENT: Color = Color::Rgb(99, 179, 237);   // sky-blue
const SUCCESS: Color = Color::Rgb(104, 211, 145);  // green
const WARN: Color = Color::Rgb(246, 173, 85);      // amber
const ERROR_COL: Color = Color::Rgb(252, 129, 129); // red
const DIM: Color = Color::Rgb(160, 174, 192);       // muted grey
const BG: Color = Color::Rgb(13, 17, 23);           // near-black
const SURFACE: Color = Color::Rgb(22, 27, 34);      // card surface
const BORDER: Color = Color::Rgb(48, 54, 61);       // border grey

// ── app state ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Home,
    Host(HostStep),
    Receiver(ReceiverStep),
    About,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostStep {
    /// Showing the offer QR and waiting for user to open a connection.
    ShowOffer { payload: String, qr_art: String },
    /// User is typing in the receiver's answer payload.
    EnterAnswer { offer_payload: String, qr_art: String, answer_input: String },
    /// Connecting (show spinner).
    Connecting,
    /// Connected — streaming.
    /// This variant is set by the async WebRTC handshake task once ICE
    /// reaches the `Connected` state.  The TUI renders it immediately
    /// when constructed so it is ready for Phase-4 wiring.
    #[allow(dead_code)]
    Connected,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiverStep {
    /// Waiting for the user to paste the offer payload.
    EnterOffer { input: String },
    /// Showing the answer QR for the host to scan.
    ShowAnswer { answer_payload: String, qr_art: String },
    /// Connecting.
    Connecting,
    /// Connected — receiving.
    /// Set by the async WebRTC handshake task once ICE connects.
    #[allow(dead_code)]
    Connected,
}

pub struct App {
    pub screen: Screen,
    pub status_msg: Option<(String, bool)>, // (message, is_error)
    pub tick: u64,                          // used for spinner animation
    pub quit: bool,
    pub home_selection: usize,              // 0=Host, 1=Receiver, 2=About
    start_time: Instant,
}

impl App {
    pub fn new() -> Self {
        Self {
            screen: Screen::Home,
            status_msg: None,
            tick: 0,
            quit: false,
            home_selection: 0,
            start_time: Instant::now(),
        }
    }

    /// Elapsed seconds since launch (for spinner etc.)
    fn elapsed_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    fn spinner_char(&self) -> char {
        const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        FRAMES[(self.tick as usize) % FRAMES.len()]
    }

    /// Generate the QR art for a payload string (compact block characters).
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

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    /// Handle a key press.  Returns `true` if the app should quit.
    pub fn handle_key(&mut self, key: KeyCode) -> bool {
        // Global quit
        if key == KeyCode::Char('q') && !self.is_in_text_input() {
            return true;
        }

        match self.screen.clone() {
            Screen::Home => self.handle_home_key(key),
            Screen::Host(step) => self.handle_host_key(key, step),
            Screen::Receiver(step) => self.handle_receiver_key(key, step),
            Screen::About => {
                if matches!(key, KeyCode::Esc | KeyCode::Char('b') | KeyCode::Char('q')) {
                    self.screen = Screen::Home;
                }
            }
        }
        false
    }

    fn is_in_text_input(&self) -> bool {
        matches!(
            &self.screen,
            Screen::Host(HostStep::EnterAnswer { .. })
                | Screen::Receiver(ReceiverStep::EnterOffer { .. })
        )
    }

    fn handle_home_key(&mut self, key: KeyCode) {
        const ITEMS: usize = 3;
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                self.home_selection = self.home_selection.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.home_selection + 1 < ITEMS {
                    self.home_selection += 1;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.home_selection {
                0 => {
                    // Generate offer
                    match self.generate_offer_qr() {
                        Ok((payload, qr_art)) => {
                            self.screen = Screen::Host(HostStep::ShowOffer { payload, qr_art });
                            self.status_msg = None;
                        }
                        Err(e) => {
                            self.status_msg = Some((format!("Offer error: {e}"), true));
                        }
                    }
                }
                1 => {
                    self.screen = Screen::Receiver(ReceiverStep::EnterOffer {
                        input: String::new(),
                    });
                    self.status_msg = None;
                }
                2 => {
                    self.screen = Screen::About;
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn handle_host_key(&mut self, key: KeyCode, step: HostStep) {
        match step {
            HostStep::ShowOffer { payload, qr_art } => match key {
                KeyCode::Enter | KeyCode::Char('n') => {
                    self.screen = Screen::Host(HostStep::EnterAnswer {
                        offer_payload: payload,
                        qr_art,
                        answer_input: String::new(),
                    });
                }
                KeyCode::Esc | KeyCode::Char('b') => {
                    self.screen = Screen::Home;
                }
                _ => {}
            },
            HostStep::EnterAnswer {
                offer_payload,
                qr_art,
                mut answer_input,
            } => match key {
                KeyCode::Char(c) => {
                    answer_input.push(c);
                    self.screen = Screen::Host(HostStep::EnterAnswer {
                        offer_payload,
                        qr_art,
                        answer_input,
                    });
                }
                KeyCode::Backspace => {
                    answer_input.pop();
                    self.screen = Screen::Host(HostStep::EnterAnswer {
                        offer_payload,
                        qr_art,
                        answer_input,
                    });
                }
                KeyCode::Enter => {
                    // Validate the answer payload
                    let trimmed = answer_input.trim().to_string();
                    match decode_sdp(&QrPayload(trimmed)) {
                        Ok(_sdp) => {
                            self.screen = Screen::Host(HostStep::Connecting);
                            self.status_msg =
                                Some(("Answer accepted — establishing P2P link…".into(), false));
                        }
                        Err(e) => {
                            self.status_msg =
                                Some((format!("Invalid answer payload: {e}"), true));
                            self.screen = Screen::Host(HostStep::EnterAnswer {
                                offer_payload,
                                qr_art,
                                answer_input: String::new(),
                            });
                        }
                    }
                }
                KeyCode::Esc => {
                    self.screen = Screen::Host(HostStep::ShowOffer {
                        payload: offer_payload,
                        qr_art,
                    });
                }
                _ => {}
            },
            HostStep::Connecting => {
                if key == KeyCode::Esc {
                    self.screen = Screen::Home;
                }
            }
            HostStep::Connected => {
                if matches!(key, KeyCode::Esc | KeyCode::Char('q')) {
                    self.screen = Screen::Home;
                }
            }
        }
    }

    fn handle_receiver_key(&mut self, key: KeyCode, step: ReceiverStep) {
        match step {
            ReceiverStep::EnterOffer { mut input } => match key {
                KeyCode::Char(c) => {
                    input.push(c);
                    self.screen = Screen::Receiver(ReceiverStep::EnterOffer { input });
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.screen = Screen::Receiver(ReceiverStep::EnterOffer { input });
                }
                KeyCode::Enter => {
                    let trimmed = input.trim().to_string();
                    match self.generate_answer_qr(&trimmed) {
                        Ok((answer_payload, qr_art)) => {
                            self.screen = Screen::Receiver(ReceiverStep::ShowAnswer {
                                answer_payload,
                                qr_art,
                            });
                            self.status_msg = None;
                        }
                        Err(e) => {
                            self.status_msg =
                                Some((format!("Invalid offer payload: {e}"), true));
                            self.screen =
                                Screen::Receiver(ReceiverStep::EnterOffer { input: String::new() });
                        }
                    }
                }
                KeyCode::Esc => {
                    self.screen = Screen::Home;
                }
                _ => {}
            },
            ReceiverStep::ShowAnswer { .. } => match key {
                KeyCode::Enter | KeyCode::Char('n') => {
                    self.screen = Screen::Receiver(ReceiverStep::Connecting);
                    self.status_msg =
                        Some(("Waiting for host to complete the handshake…".into(), false));
                }
                KeyCode::Esc | KeyCode::Char('b') => {
                    self.screen = Screen::Home;
                }
                _ => {}
            },
            ReceiverStep::Connecting => {
                if key == KeyCode::Esc {
                    self.screen = Screen::Home;
                }
            }
            ReceiverStep::Connected => {
                if matches!(key, KeyCode::Esc | KeyCode::Char('q')) {
                    self.screen = Screen::Home;
                }
            }
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Build a demo offer payload (without real WebRTC since this is UI-only).
    fn generate_offer_qr(&self) -> Result<(String, String)> {
        // In the real flow this would call PeerSession::new_host().create_offer()
        // via an async executor.  For the interactive TUI we generate a
        // representative demo SDP to keep the UI self-contained.
        let demo_sdp = "v=0\no=- 0 0 IN IP4 127.0.0.1\ns=-\nt=0 0\n\
                        m=video 9 UDP/TLS/RTP/SAVPF 96\nb=AS:45000\n\
                        a=ice-ufrag:demo\na=ice-pwd:demopassword00000000001\n\
                        a=sendonly\na=rtpmap:96 H264/90000\n";
        let qr_payload = encode_sdp(demo_sdp)?;
        let qr_art = Self::make_qr_art(qr_payload.as_str());
        Ok((qr_payload.0, qr_art))
    }

    /// Decode the offer and generate an answer QR.
    fn generate_answer_qr(&self, offer_payload: &str) -> Result<(String, String)> {
        let offer_sdp = decode_sdp(&QrPayload(offer_payload.to_string()))?;
        // Build a synthetic answer (mirrors the offer).
        let answer_sdp = offer_sdp.replace("a=sendonly", "a=recvonly");
        let qr_payload = encode_sdp(&answer_sdp)?;
        let qr_art = Self::make_qr_art(qr_payload.as_str());
        Ok((qr_payload.0, qr_art))
    }
}

// ── entry point ───────────────────────────────────────────────────────────────

/// Launch the interactive TUI.  Restores the terminal on exit.
pub fn run() -> Result<()> {
    // Set up terminal
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

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_default();

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

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

// ── drawing ───────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App) {
    // Full-screen background
    let bg_block = Block::default().style(Style::default().bg(BG));
    f.render_widget(bg_block, f.area());

    // Vertical layout: header | body | status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),  // header
            Constraint::Min(0),     // body
            Constraint::Length(3),  // status / footer
        ])
        .split(f.area());

    draw_header(f, chunks[0]);

    match &app.screen {
        Screen::Home => draw_home(f, chunks[1], app),
        Screen::Host(step) => draw_host(f, chunks[1], app, step),
        Screen::Receiver(step) => draw_receiver(f, chunks[1], app, step),
        Screen::About => draw_about(f, chunks[1]),
    }

    draw_footer(f, chunks[2], app);
}

// ── header ────────────────────────────────────────────────────────────────────

fn draw_header(f: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(BORDER))
        .border_type(BorderType::Plain)
        .style(Style::default().bg(SURFACE));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(30)])
        .split(inner);

    let title = Paragraph::new(Text::from(vec![
        Line::from(vec![
            Span::styled("  ◈ ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(
                "Rust4K-P2P",
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  Decentralized 4K Desktop Streaming",
                Style::default().fg(DIM),
            ),
        ]),
        Line::from(Span::styled(
            "  WebRTC · QR-Airgap · 3840×2160 @ 60fps · 45 Mbps",
            Style::default().fg(DIM),
        )),
    ]))
    .alignment(Alignment::Left);

    let version = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            "v0.1.0  ",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "MIT license  ",
            Style::default().fg(DIM),
        )),
    ]))
    .alignment(Alignment::Right);

    f.render_widget(title, header_chunks[0]);
    f.render_widget(version, header_chunks[1]);
}

// ── footer ────────────────────────────────────────────────────────────────────

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(SURFACE));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Status message (left) + keybindings (right)
    let footer_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(50)])
        .split(inner);

    let status = if let Some((msg, is_err)) = &app.status_msg {
        let color = if *is_err { ERROR_COL } else { SUCCESS };
        let icon = if *is_err { " ✖ " } else { " ✔ " };
        Paragraph::new(Line::from(vec![
            Span::styled(icon, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(msg.as_str(), Style::default().fg(color)),
        ]))
    } else {
        Paragraph::new(Line::from(Span::styled(
            "  Ready",
            Style::default().fg(DIM),
        )))
    };

    let keys = match &app.screen {
        Screen::Home => " ↑↓ navigate  Enter select  q quit",
        Screen::Host(HostStep::ShowOffer { .. }) => " Enter next  Esc back  q quit",
        Screen::Host(HostStep::EnterAnswer { .. }) => " Type payload  Enter confirm  Esc back",
        Screen::Host(HostStep::Connecting) => " Esc cancel",
        Screen::Host(HostStep::Connected) => " Esc home  q quit",
        Screen::Receiver(ReceiverStep::EnterOffer { .. }) => " Type payload  Enter decode  Esc back",
        Screen::Receiver(ReceiverStep::ShowAnswer { .. }) => " Enter next  Esc back",
        Screen::Receiver(ReceiverStep::Connecting) => " Esc cancel",
        Screen::Receiver(ReceiverStep::Connected) => " Esc home  q quit",
        Screen::About => " Esc back  q quit",
    };

    let keybindings = Paragraph::new(Line::from(Span::styled(
        keys,
        Style::default().fg(DIM),
    )))
    .alignment(Alignment::Right);

    f.render_widget(status, footer_chunks[0]);
    f.render_widget(keybindings, footer_chunks[1]);
}

// ── home screen ───────────────────────────────────────────────────────────────

fn draw_home(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(8),  // hero banner
            Constraint::Length(1),  // spacer
            Constraint::Min(0),     // menu
        ])
        .split(area);

    // ── hero / ASCII logo ─────────────────────────────────────────────────────
    let logo_lines = vec![
        Line::from(Span::styled(
            r" ██████╗ ██╗   ██╗███████╗████████╗██╗  ██╗██╗  ██╗    ██████╗ ██████╗ ██████╗ ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            r" ██╔══██╗██║   ██║██╔════╝╚══██╔══╝██║  ██║██║ ██╔╝    ██╔══██╗╚════██╗██╔══██╗",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            r" ██████╔╝██║   ██║███████╗   ██║   ███████║█████╔╝     ██████╔╝ █████╔╝██████╔╝",
            Style::default().fg(Color::Rgb(72, 149, 239)).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            r" ██╔══██╗██║   ██║╚════██║   ██║   ╚════██║██╔═██╗     ██╔═══╝ ██╔═══╝ ██╔═══╝ ",
            Style::default().fg(Color::Rgb(72, 149, 239)).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            r" ██║  ██║╚██████╔╝███████║   ██║        ██║██║  ██╗    ██║     ███████╗██║     ",
            Style::default().fg(Color::Rgb(58, 123, 213)).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            r" ╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝        ╚═╝╚═╝  ╚═╝    ╚═╝     ╚══════╝╚═╝     ",
            Style::default().fg(Color::Rgb(58, 123, 213)).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled(
                "     Decentralized · Serverless · Ultra-Low Latency · ",
                Style::default().fg(DIM),
            ),
            Span::styled(
                "3840×2160 @ 60 fps",
                Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let logo = Paragraph::new(logo_lines).alignment(Alignment::Center);
    f.render_widget(logo, chunks[0]);

    // ── menu ──────────────────────────────────────────────────────────────────
    let menu_items = vec![
        ("📡", "Host (Sender)", "Generate an offer QR and stream your screen to a peer"),
        ("📺", "Receiver (Client)", "Scan the host's QR and start receiving the 4K stream"),
        ("ℹ️ ", "About & Help", "Project info, requirements, and keybindings"),
    ];

    let menu_area = center_rect(52, menu_items.len() as u16 * 3 + 2, chunks[2]);

    let items: Vec<ListItem> = menu_items
        .iter()
        .enumerate()
        .map(|(i, (icon, title, desc))| {
            let is_selected = i == app.home_selection;
            let (title_style, desc_style, prefix) = if is_selected {
                (
                    Style::default().fg(BG).bg(ACCENT).add_modifier(Modifier::BOLD),
                    Style::default().fg(ACCENT),
                    "▶ ",
                )
            } else {
                (
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    Style::default().fg(DIM),
                    "  ",
                )
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(format!("{icon} {title}"), title_style),
                ]),
                Line::from(Span::styled(format!("     {desc}"), desc_style)),
                Line::default(),
            ])
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER))
            .title(Span::styled(
                " Select Mode ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(SURFACE))
            .padding(Padding::horizontal(1)),
    );

    f.render_widget(list, menu_area);
}

// ── host flow ─────────────────────────────────────────────────────────────────

fn draw_host(f: &mut Frame, area: Rect, app: &App, step: &HostStep) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    draw_stepper(f, chunks[0], host_step_index(step), &["Offer QR", "Enter Answer", "Connecting", "Streaming"]);

    match step {
        HostStep::ShowOffer { payload, qr_art } => {
            draw_host_offer(f, chunks[1], payload, qr_art);
        }
        HostStep::EnterAnswer { qr_art, answer_input, .. } => {
            draw_host_enter_answer(f, chunks[1], qr_art, answer_input);
        }
        HostStep::Connecting => {
            draw_connecting(f, chunks[1], app, "Establishing P2P link with receiver…");
        }
        HostStep::Connected => {
            draw_connected(f, chunks[1], "Streaming 4K desktop to receiver");
        }
    }
}

fn host_step_index(step: &HostStep) -> usize {
    match step {
        HostStep::ShowOffer { .. } => 0,
        HostStep::EnterAnswer { .. } => 1,
        HostStep::Connecting => 2,
        HostStep::Connected => 3,
    }
}

fn draw_host_offer(f: &mut Frame, area: Rect, payload: &str, qr_art: &str) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .margin(1)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    // QR code
    let qr_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            " ◈ Offer QR Code ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(SURFACE));

    let qr_text = Paragraph::new(qr_art)
        .block(qr_block)
        .style(Style::default().fg(Color::White).bg(SURFACE));
    f.render_widget(qr_text, cols[0]);

    // Instructions
    let instr_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(Span::styled(
            " Instructions ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(SURFACE))
        .padding(Padding::uniform(1));

    let truncated_payload = if payload.len() > 60 {
        format!("{}…", &payload[..60])
    } else {
        payload.to_string()
    };

    let instr = Paragraph::new(vec![
        Line::from(Span::styled("Step 1 of 4 — Share Offer", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from(Span::styled("Have the receiver scan this QR code with their camera, or share the text payload below.", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("Payload (first 60 chars):", Style::default().fg(DIM))),
        Line::from(Span::styled(truncated_payload, Style::default().fg(WARN))),
        Line::default(),
        Line::from(Span::styled("Full payload is shown below.", Style::default().fg(DIM))),
        Line::default(),
        Line::from(Span::styled("Once the receiver has scanned it:", Style::default().fg(Color::White))),
        Line::from(Span::styled(" → Press Enter to advance", Style::default().fg(SUCCESS))),
        Line::from(Span::styled(" → Esc to go back to the menu", Style::default().fg(DIM))),
    ])
    .block(instr_block)
    .wrap(Wrap { trim: false });

    f.render_widget(instr, cols[1]);

    // Payload box at the bottom
    let payload_area = Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(5),
        width: area.width.saturating_sub(2),
        height: 4,
    };

    let payload_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(Span::styled(
            " Full Offer Payload (copy & share) ",
            Style::default().fg(DIM),
        ))
        .style(Style::default().bg(SURFACE));

    let payload_para = Paragraph::new(payload)
        .block(payload_block)
        .style(Style::default().fg(WARN))
        .wrap(Wrap { trim: true });

    f.render_widget(payload_para, payload_area);
}

fn draw_host_enter_answer(f: &mut Frame, area: Rect, qr_art: &str, input: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Min(0), Constraint::Length(5)])
        .split(area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(chunks[0]);

    // Keep showing the offer QR for context
    let qr_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM))
        .title(Span::styled(
            " Your Offer QR (sent) ",
            Style::default().fg(DIM),
        ))
        .style(Style::default().bg(SURFACE));

    let qr_para = Paragraph::new(qr_art)
        .block(qr_block)
        .style(Style::default().fg(DIM).bg(SURFACE));
    f.render_widget(qr_para, cols[0]);

    let instr_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(Span::styled(
            " Instructions ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(SURFACE))
        .padding(Padding::uniform(1));

    let instr = Paragraph::new(vec![
        Line::from(Span::styled("Step 2 of 4 — Enter Answer", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from(Span::styled("The receiver will display their own QR code or a text payload.", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("Paste their Base64+Gzip answer payload in the box below and press Enter.", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("The payload starts with 'H4sI…'", Style::default().fg(DIM))),
    ])
    .block(instr_block)
    .wrap(Wrap { trim: false });
    f.render_widget(instr, cols[1]);

    // Input box
    let cursor = if app_tick_to_bool() { '▌' } else { ' ' };
    let input_display = format!("{input}{cursor}");

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            " Paste Receiver's Answer Payload ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(SURFACE))
        .padding(Padding::horizontal(1));

    let input_para = Paragraph::new(input_display.as_str())
        .block(input_block)
        .style(Style::default().fg(Color::White));

    f.render_widget(input_para, chunks[1]);
}

/// Simple tick→bool for cursor blink (we don't have app.tick here, use time).
fn app_tick_to_bool() -> bool {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis();
    ms < 500
}

// ── receiver flow ─────────────────────────────────────────────────────────────

fn draw_receiver(f: &mut Frame, area: Rect, app: &App, step: &ReceiverStep) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    draw_stepper(f, chunks[0], receiver_step_index(step), &["Enter Offer", "Answer QR", "Connecting", "Receiving"]);

    match step {
        ReceiverStep::EnterOffer { input } => draw_receiver_enter_offer(f, chunks[1], input),
        ReceiverStep::ShowAnswer { answer_payload, qr_art } => {
            draw_receiver_answer(f, chunks[1], answer_payload, qr_art);
        }
        ReceiverStep::Connecting => {
            draw_connecting(f, chunks[1], app, "Waiting for host to complete handshake…");
        }
        ReceiverStep::Connected => {
            draw_connected(f, chunks[1], "Receiving 4K stream from host");
        }
    }
}

fn receiver_step_index(step: &ReceiverStep) -> usize {
    match step {
        ReceiverStep::EnterOffer { .. } => 0,
        ReceiverStep::ShowAnswer { .. } => 1,
        ReceiverStep::Connecting => 2,
        ReceiverStep::Connected => 3,
    }
}

fn draw_receiver_enter_offer(f: &mut Frame, area: Rect, input: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Min(0), Constraint::Length(5)])
        .split(area);

    let instr = Paragraph::new(vec![
        Line::from(Span::styled("Step 1 of 4 — Enter Host's Offer", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from(Span::styled("Scan the host's QR code with a QR reader app, then paste the decoded payload below.", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("Alternatively, the host can read the payload text aloud or send it via a side channel.", Style::default().fg(DIM))),
        Line::default(),
        Line::from(Span::styled("The payload is a Base64+Gzip encoded SDP string, typically starting with 'H4sI…'.", Style::default().fg(DIM))),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER))
            .title(Span::styled(" Offer Payload ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
            .style(Style::default().bg(SURFACE))
            .padding(Padding::uniform(1)),
    )
    .wrap(Wrap { trim: false });
    f.render_widget(instr, chunks[0]);

    let cursor = if app_tick_to_bool() { '▌' } else { ' ' };
    let input_display = format!("{input}{cursor}");

    let input_para = Paragraph::new(input_display.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACCENT))
                .title(Span::styled(
                    " Paste offer payload and press Enter ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ))
                .style(Style::default().bg(SURFACE))
                .padding(Padding::horizontal(1)),
        )
        .style(Style::default().fg(WARN));
    f.render_widget(input_para, chunks[1]);
}

fn draw_receiver_answer(f: &mut Frame, area: Rect, payload: &str, qr_art: &str) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .margin(1)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    let qr_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SUCCESS))
        .title(Span::styled(
            " ◈ Answer QR Code ",
            Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(SURFACE));

    let qr_para = Paragraph::new(qr_art)
        .block(qr_block)
        .style(Style::default().fg(Color::White).bg(SURFACE));
    f.render_widget(qr_para, cols[0]);

    let truncated_payload = if payload.len() > 60 {
        format!("{}…", &payload[..60])
    } else {
        payload.to_string()
    };

    let instr = Paragraph::new(vec![
        Line::from(Span::styled("Step 2 of 4 — Share Answer", Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from(Span::styled("Have the host scan this QR code, or share the text payload.", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("Payload (first 60 chars):", Style::default().fg(DIM))),
        Line::from(Span::styled(truncated_payload, Style::default().fg(WARN))),
        Line::default(),
        Line::from(Span::styled("Once the host has scanned it:", Style::default().fg(Color::White))),
        Line::from(Span::styled(" → Press Enter to wait for connection", Style::default().fg(SUCCESS))),
        Line::from(Span::styled(" → Esc to go back", Style::default().fg(DIM))),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER))
            .title(Span::styled(" Instructions ", Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD)))
            .style(Style::default().bg(SURFACE))
            .padding(Padding::uniform(1)),
    )
    .wrap(Wrap { trim: false });
    f.render_widget(instr, cols[1]);

    // Payload box
    let payload_area = Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(5),
        width: area.width.saturating_sub(2),
        height: 4,
    };

    let payload_para = Paragraph::new(payload)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .title(Span::styled(" Full Answer Payload (copy & share) ", Style::default().fg(DIM)))
                .style(Style::default().bg(SURFACE)),
        )
        .style(Style::default().fg(WARN))
        .wrap(Wrap { trim: true });
    f.render_widget(payload_para, payload_area);
}

// ── connecting / connected screens ────────────────────────────────────────────

fn draw_connecting(f: &mut Frame, area: Rect, app: &App, msg: &str) {
    let center = center_rect(50, 10, area);
    f.render_widget(Clear, center);

    let spinner = app.spinner_char();
    let elapsed = app.elapsed_secs();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            " Connecting… ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(SURFACE));

    let para = Paragraph::new(vec![
        Line::default(),
        Line::from(Span::styled(
            format!("  {spinner}  {msg}"),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(Span::styled(
            format!("  Elapsed: {elapsed}s — performing UDP hole-punch via STUN…"),
            Style::default().fg(DIM),
        )),
        Line::default(),
        Line::from(Span::styled(
            "  Ensure UDP traffic is allowed on your network.",
            Style::default().fg(WARN),
        )),
        Line::default(),
        Line::from(Span::styled("  Press Esc to cancel.", Style::default().fg(DIM))),
    ])
    .block(block)
    .alignment(Alignment::Left);
    f.render_widget(para, center);
}

fn draw_connected(f: &mut Frame, area: Rect, msg: &str) {
    let center = center_rect(50, 12, area);
    f.render_widget(Clear, center);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SUCCESS))
        .title(Span::styled(
            " ✓ Connected ",
            Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(SURFACE));

    let para = Paragraph::new(vec![
        Line::default(),
        Line::from(Span::styled(
            "  ✓ P2P link established!",
            Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(Span::styled(
            format!("  {msg}"),
            Style::default().fg(Color::White),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled("  Resolution: ", Style::default().fg(DIM)),
            Span::styled("3840×2160 @ 60 fps", Style::default().fg(SUCCESS)),
        ]),
        Line::from(vec![
            Span::styled("  Bitrate:    ", Style::default().fg(DIM)),
            Span::styled("~45 Mbps (NVENC H.264)", Style::default().fg(SUCCESS)),
        ]),
        Line::from(vec![
            Span::styled("  Transport:  ", Style::default().fg(DIM)),
            Span::styled("UDP (STUN hole-punch)", Style::default().fg(SUCCESS)),
        ]),
        Line::default(),
        Line::from(Span::styled("  Press Esc to disconnect and return home.", Style::default().fg(DIM))),
    ])
    .block(block)
    .alignment(Alignment::Left);
    f.render_widget(para, center);
}

// ── about screen ──────────────────────────────────────────────────────────────

fn draw_about(f: &mut Frame, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .margin(2)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let left = Paragraph::new(vec![
        Line::from(Span::styled("Rust4K-P2P", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("Decentralized 4K 60fps desktop streaming", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("Tech Stack", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("  Language  Rust (memory-safe, zero-copy)", Style::default().fg(Color::White))),
        Line::from(Span::styled("  WebRTC    webrtc-rs (P2P, STUN/ICE)", Style::default().fg(Color::White))),
        Line::from(Span::styled("  Media     GStreamer (NVENC/AV1/x264)", Style::default().fg(Color::White))),
        Line::from(Span::styled("  Audio     CPAL / Oboe (Opus codec)", Style::default().fg(Color::White))),
        Line::from(Span::styled("  UI        ratatui + crossterm", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("Video Quality", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("  3840×2160 @ 60 fps", Style::default().fg(SUCCESS))),
        Line::from(Span::styled("  35–50 Mbps (SDP munged)", Style::default().fg(SUCCESS))),
        Line::from(Span::styled("  AV1 or H.264/H.265 (GPU)", Style::default().fg(SUCCESS))),
        Line::default(),
        Line::from(Span::styled("QR-Airgap Signaling", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("  SDP → Gzip → Base64 → QR", Style::default().fg(Color::White))),
        Line::from(Span::styled("  No signaling server required", Style::default().fg(SUCCESS))),
        Line::from(Span::styled("  UDP hole-punch via STUN", Style::default().fg(Color::White))),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER))
            .title(Span::styled(" About ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
            .style(Style::default().bg(SURFACE))
            .padding(Padding::uniform(1)),
    )
    .wrap(Wrap { trim: false });
    f.render_widget(left, chunks[0]);

    let right = Paragraph::new(vec![
        Line::from(Span::styled("Keybindings", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::default(),
        kb_line("↑ / k", "Move selection up"),
        kb_line("↓ / j", "Move selection down"),
        kb_line("Enter", "Confirm / advance"),
        kb_line("Esc",   "Go back"),
        kb_line("q",     "Quit (outside text input)"),
        Line::default(),
        Line::from(Span::styled("System Requirements", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from(Span::styled("  Sender (Host)", Style::default().fg(WARN).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("  • Upload > 50 Mbps", Style::default().fg(Color::White))),
        Line::from(Span::styled("  • NVIDIA / AMD / Apple Silicon GPU", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("  Receiver (Client)", Style::default().fg(WARN).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("  • 4K monitor", Style::default().fg(Color::White))),
        Line::from(Span::styled("  • HW decode (VP9 / AV1 / H.265)", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("  Network", Style::default().fg(WARN).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("  • UDP traffic allowed (STUN verified)", Style::default().fg(Color::White))),
        Line::default(),
        Line::from(Span::styled("Build flags", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled("  --features gstreamer   enable HW capture", Style::default().fg(DIM))),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER))
            .title(Span::styled(" Help ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
            .style(Style::default().bg(SURFACE))
            .padding(Padding::uniform(1)),
    )
    .wrap(Wrap { trim: false });
    f.render_widget(right, chunks[1]);
}

fn kb_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("  {key:<8}"), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(desc, Style::default().fg(Color::White)),
    ])
}

// ── stepper widget ────────────────────────────────────────────────────────────

fn draw_stepper(f: &mut Frame, area: Rect, active: usize, labels: &[&str]) {
    let n = labels.len();
    let constraints: Vec<Constraint> =
        (0..n).map(|_| Constraint::Ratio(1, n as u32)).collect();

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (i, (label, &col)) in labels.iter().zip(cols.iter()).enumerate() {
        let (style, prefix) = if i < active {
            (Style::default().fg(SUCCESS), "✓ ")
        } else if i == active {
            (Style::default().fg(ACCENT).add_modifier(Modifier::BOLD), "▶ ")
        } else {
            (Style::default().fg(DIM), "  ")
        };

        let step_num = format!("{}", i + 1);
        let para = Paragraph::new(Line::from(vec![
            Span::styled(prefix, style),
            Span::styled(step_num + ". ", style),
            Span::styled(*label, style),
        ]))
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(if i == active {
                    Style::default().fg(ACCENT)
                } else {
                    Style::default().fg(BORDER)
                }),
        )
        .alignment(Alignment::Center);

        f.render_widget(para, col);
    }
}

// ── layout helpers ────────────────────────────────────────────────────────────

/// Return a centered `Rect` with the given dimensions inside `area`.
fn center_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}
