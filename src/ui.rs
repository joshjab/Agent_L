use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect, Alignment},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Paragraph, Widget, Wrap},
};
use crate::agents::orchestrator::{AgentKind, IntentType, TaskPlan};
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

        // Status line: model, tokens, optional routing decision
        let mut status_spans: Vec<Span> = vec![
            " MODEL: ".into(),
            Span::styled(self.model_name.clone(), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            " | TOKENS: ".into(),
            Span::styled(self.token_count.to_string(), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        ];
        if let Some(plan) = &self.route_decision {
            status_spans.push(" | ".into());
            status_spans.push(Span::styled(
                format_route_decision(plan),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
        }
        status_spans.push(" | [Ctrl+Q] Quit ".into());
        Paragraph::new(Line::from(status_spans)).render(chunks[2], buf);
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

/// Format a `TaskPlan` into a compact status-line string.
///
/// Examples:
/// - Conversational → `"Agent L → Chat"`
/// - Factual        → `"Agent L → Search (Factual)"`
/// - Creative       → `"Agent L → Chat (Creative)"`
/// - Task (multi)   → `"Agent L → Shell + Code"`
pub fn format_route_decision(plan: &TaskPlan) -> String {
    let mut seen = std::collections::HashSet::new();
    let agents: Vec<&str> = plan
        .steps
        .iter()
        .filter_map(|s| {
            let label = agent_kind_label(&s.agent);
            if seen.insert(label) { Some(label) } else { None }
        })
        .collect();

    let agents_str = agents.join(" + ");

    match plan.intent_type {
        IntentType::Factual  => format!("Agent L \u{2192} {agents_str} (Factual)"),
        IntentType::Creative => format!("Agent L \u{2192} {agents_str} (Creative)"),
        _                    => format!("Agent L \u{2192} {agents_str}"),
    }
}

fn agent_kind_label(kind: &AgentKind) -> &'static str {
    match kind {
        AgentKind::Chat     => "Chat",
        AgentKind::Code     => "Code",
        AgentKind::Search   => "Search",
        AgentKind::Shell    => "Shell",
        AgentKind::Calendar => "Calendar",
        AgentKind::Memory   => "Memory",
        AgentKind::Unknown  => "Unknown",
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::orchestrator::{AgentKind, IntentType, PlanStep, TaskPlan};

    fn plan(intent: IntentType, agents: &[AgentKind]) -> TaskPlan {
        TaskPlan {
            intent_type: intent,
            steps: agents
                .iter()
                .map(|a| PlanStep { agent: a.clone(), task: "x".into(), depends_on: None })
                .collect(),
        }
    }

    #[test]
    fn route_conversational_shows_no_intent_suffix() {
        let s = format_route_decision(&plan(IntentType::Conversational, &[AgentKind::Chat]));
        assert_eq!(s, "Agent L → Chat");
    }

    #[test]
    fn route_factual_shows_factual_suffix() {
        let s = format_route_decision(&plan(IntentType::Factual, &[AgentKind::Search]));
        assert_eq!(s, "Agent L → Search (Factual)");
    }

    #[test]
    fn route_creative_shows_creative_suffix() {
        let s = format_route_decision(&plan(IntentType::Creative, &[AgentKind::Chat]));
        assert_eq!(s, "Agent L → Chat (Creative)");
    }

    #[test]
    fn route_task_multi_step_joins_with_plus() {
        let s = format_route_decision(&plan(IntentType::Task, &[AgentKind::Shell, AgentKind::Code]));
        assert_eq!(s, "Agent L → Shell + Code");
    }

    #[test]
    fn route_deduplicates_repeated_agents() {
        // Two steps with the same agent should appear only once
        let s = format_route_decision(&plan(IntentType::Task, &[AgentKind::Chat, AgentKind::Chat]));
        assert_eq!(s, "Agent L → Chat");
    }

    #[test]
    fn route_preserves_agent_order() {
        let s = format_route_decision(&plan(IntentType::Task, &[AgentKind::Search, AgentKind::Shell, AgentKind::Code]));
        assert_eq!(s, "Agent L → Search + Shell + Code");
    }

    #[test]
    fn test_plain_text() {
        let lines = parse_simple_markdown("hello world");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content, "hello world");
    }

    #[test]
    fn test_bold_single_pair() {
        let lines = parse_simple_markdown("this is **bold** text");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 3);
        assert_eq!(lines[0].spans[0].content, "this is ");
        assert_eq!(lines[0].spans[1].content, "bold");
        assert_eq!(lines[0].spans[2].content, " text");
    }

    #[test]
    fn test_bold_multiple_pairs() {
        let lines = parse_simple_markdown("**A** and **B**");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 5);
    }

    #[test]
    fn test_multiline() {
        let lines = parse_simple_markdown("line one\n**line two**\nline three");
        assert_eq!(lines.len(), 3);
        // line 0: plain
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content, "line one");
        // line 1: bold
        assert_eq!(lines[1].spans.len(), 3);
        assert_eq!(lines[1].spans[1].content, "line two");
        // line 2: plain
        assert_eq!(lines[2].spans.len(), 1);
        assert_eq!(lines[2].spans[0].content, "line three");
    }

    #[test]
    fn test_empty_string() {
        let lines = parse_simple_markdown("");
        assert_eq!(lines.len(), 0);
    }

    #[test]
    fn test_only_bold() {
        // "**only bold**" splits to ["", "only bold", ""] → 3 spans
        let lines = parse_simple_markdown("**only bold**");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 3);
        assert_eq!(lines[0].spans[1].content, "only bold");
    }

    #[test]
    fn test_adjacent_markers() {
        // "****" splits to ["", "", ""] → 3 spans, no panic
        let lines = parse_simple_markdown("****");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 3);
    }

    #[test]
    fn test_unclosed_bold() {
        // "a **b" splits to ["a ", "b"] → raw, bold — documents known behavior
        let lines = parse_simple_markdown("a **b");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].content, "a ");
        assert_eq!(lines[0].spans[1].content, "b");
    }

    #[test]
    fn test_spinner_has_10_frames() {
        assert_eq!(SPINNER.len(), 10);
    }
}
