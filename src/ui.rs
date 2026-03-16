use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect, Alignment},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Paragraph, Widget, Wrap},
};
use crate::app::{App, Role, StartupState};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::default()
            .constraints([
                Constraint::Min(0),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);

        if self.startup_state != StartupState::Ready {
            render_startup(self, chunks[0], buf);
        } else {
            render_chat(self, chunks[0], buf);
        }

        // Prompt Block
        if self.startup_state != StartupState::Ready {
            Paragraph::new(Span::styled("(waiting for Ollama...)", Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)))
                .block(Block::bordered().title(" Prompt ").border_style(Style::default().fg(Color::DarkGray)))
                .render(chunks[1], buf);
        } else {
            Paragraph::new(format!("> {}", self.input))
                .block(Block::bordered().title(" Prompt ").border_style(Style::default().fg(Color::Cyan)))
                .render(chunks[1], buf);
        }

        // Model and Token Count
        let status_info = Line::from(vec![
            " MODEL: ".into(),
            self.model_name.clone().yellow().bold(),
            " | TOKENS: ".into(),
            self.token_count.to_string().green().bold(),
            " | [Ctrl+Q] Quit ".into(),
        ]);
        Paragraph::new(status_info).render(chunks[2], buf);
    }
}

fn render_startup(app: &App, area: Rect, buf: &mut Buffer) {
    let spinner = SPINNER[app.tick % 10];
    let mut lines: Vec<Line> = Vec::new();

    match &app.startup_state {
        StartupState::Connecting => {
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", spinner), Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("Connecting to Ollama at {}...", app.base_url),
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        }
        StartupState::CheckingModel => {
            lines.push(Line::from(Span::styled(
                "✓ Ollama is running",
                Style::default().fg(Color::Green),
            )));
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", spinner), Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("Checking for model '{}'...", app.model_name),
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        }
        StartupState::LoadingModel => {
            lines.push(Line::from(Span::styled(
                "✓ Ollama is running",
                Style::default().fg(Color::Green),
            )));
            lines.push(Line::from(Span::styled(
                "✓ Model found",
                Style::default().fg(Color::Green),
            )));
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", spinner), Style::default().fg(Color::Yellow)),
                Span::styled(
                    "Loading model into memory...",
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        }
        StartupState::Failed(msg) => {
            lines.push(Line::from(Span::styled(
                "Startup failed:",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(Color::Red),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Press Ctrl+Q to quit",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            )));
        }
        StartupState::Ready => {}
    }

    Paragraph::new(Text::from(lines))
        .block(
            Block::bordered()
                .title(
                    Line::from(" 🦙 Agent L ")
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                )
                .title_alignment(Alignment::Center)
                .border_type(BorderType::Rounded),
        )
        .render(area, buf);
}

fn render_chat(app: &App, area: Rect, buf: &mut Buffer) {
    let mut list_items = Vec::new();
    let chat_width = area.width.saturating_sub(4);

    for msg in &app.history {
        let separator = "─".repeat(chat_width as usize);
        list_items.push(Line::from(separator.dark_gray()));

        match msg.role {
            Role::User => {
                list_items.push(Line::from(vec![
                    " You: ".bold().blue(),
                    msg.content.as_str().white(),
                ]));
            }
            Role::Assistant => {
                list_items.push(Line::from(" Ollama: ".bold().magenta()));
                list_items.extend(parse_simple_markdown(&msg.content));
            }
        }
        list_items.push(Line::from(""));
    }

    Paragraph::new(Text::from(list_items))
        .block(
            Block::bordered()
                .title(
                    Line::from(" 🦙 Agent L ")
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                )
                .title_alignment(Alignment::Center)
                .border_type(BorderType::Rounded),
        )
        .wrap(Wrap { trim: true })
        .scroll((app.scroll_offset, 0))
        .render(area, buf);
}

fn parse_simple_markdown(text: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        let mut spans = Vec::new();
        if raw_line.contains("**") {
            let parts: Vec<&str> = raw_line.split("**").collect();
            for (i, part) in parts.iter().enumerate() {
                if i % 2 == 1 {
                    spans.push(Span::styled(*part, Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow)));
                } else {
                    spans.push(Span::raw(*part));
                }
            }
        } else {
            spans.push(Span::raw(raw_line));
        }
        lines.push(Line::from(spans));
    }
    lines
}
