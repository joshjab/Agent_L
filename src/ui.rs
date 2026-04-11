use crate::agents::orchestrator::{AgentKind, IntentType, TaskPlan};
use crate::agents::specialists::code::TaskScope;
use crate::app::{App, Role, StartupState};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Paragraph, Widget, Wrap},
};

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
            Paragraph::new(Span::styled(
                "(waiting for Ollama...)",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ))
            .block(
                Block::bordered()
                    .title(" Prompt ")
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .render(chunks[1], buf);
        } else {
            Paragraph::new(format!("> {}", self.input))
                .block(
                    Block::bordered()
                        .title(" Prompt ")
                        .border_style(Style::default().fg(Color::Cyan)),
                )
                .render(chunks[1], buf);
        }

        // Status line: model, token counts (↑ prompt / ↓ generated), optional routing decision
        let mut status_spans: Vec<Span> = vec![
            " MODEL: ".into(),
            Span::styled(
                self.model_name.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            " | ctx: ".into(),
            Span::styled(
                self.context_tokens.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if self.thinking_tokens > 0 {
            status_spans.push(" | think: ".into());
            status_spans.push(Span::styled(
                self.thinking_tokens.to_string(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::DIM),
            ));
        }
        if let Some(plan) = &self.route_decision {
            status_spans.push(" | ".into());
            status_spans.push(Span::styled(
                format_route_decision(plan, self.code_scope.as_ref()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
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
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        }
        StartupState::Ready => {}
    }

    Paragraph::new(Text::from(lines))
        .block(
            Block::bordered()
                .title(
                    Line::from(" 🦙 Agent L ").style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
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
                    Line::from(" 🦙 Agent L ").style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
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
/// - Code + scope   → `"Agent L → Code (one-off)"` or `"Agent L → Code (project)"`
pub fn format_route_decision(plan: &TaskPlan, code_scope: Option<&TaskScope>) -> String {
    let mut seen = std::collections::HashSet::new();
    let agents: Vec<String> = plan
        .steps
        .iter()
        .filter_map(|s| {
            let base = agent_kind_label(&s.agent);
            if seen.insert(base) {
                // Annotate Code with its scope when known.
                if matches!(s.agent, AgentKind::Code)
                    && let Some(scope) = code_scope
                {
                    let label = match scope {
                        TaskScope::OneOff => "Code (one-off)",
                        TaskScope::Project => "Code (project)",
                    };
                    return Some(label.to_string());
                }
                Some(base.to_string())
            } else {
                None
            }
        })
        .collect();

    let agents_str = agents.join(" + ");

    match plan.intent_type {
        IntentType::Factual => format!("Agent L \u{2192} {agents_str} (Factual)"),
        IntentType::Creative => format!("Agent L \u{2192} {agents_str} (Creative)"),
        _ => format!("Agent L \u{2192} {agents_str}"),
    }
}

fn agent_kind_label(kind: &AgentKind) -> &'static str {
    match kind {
        AgentKind::Chat => "Chat",
        AgentKind::Code => "Code",
        AgentKind::Search => "Search",
        AgentKind::Shell => "Shell",
        AgentKind::Calendar => "Calendar",
        AgentKind::Memory => "Memory",
        AgentKind::Unknown => "Unknown",
    }
}

/// Split `text` into spans, styling any `https://` URLs as underlined cyan.
/// Ratatui renders Span content as literal text, so raw OSC 8 escape sequences
/// cannot be used — they would appear as garbage characters in the TUI.
fn linkify_text(text: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut remaining = text;

    while let Some(pos) = remaining.find("https://") {
        // Strip a trailing "Source: " label immediately before the URL — it is
        // redundant once the URL is already rendered as "[source]".
        let prefix = &remaining[..pos];
        let trimmed_prefix = prefix.strip_suffix("Source: ").unwrap_or(prefix);
        if !trimmed_prefix.is_empty() {
            spans.push(Span::raw(trimmed_prefix.to_string()));
        }
        let url_str = &remaining[pos..];
        // URL ends at whitespace or closing punctuation that follows a URL.
        let end = url_str
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\'' | ',' | ';'))
            .unwrap_or(url_str.len());
        // Render the URL as "[source]" rather than the raw link — the full URL
        // clutters the output and is not clickable in ratatui (OSC 8 is unsupported).
        spans.push(Span::styled(
            "[source]",
            Style::default()
                .add_modifier(Modifier::UNDERLINED)
                .fg(Color::Cyan),
        ));
        remaining = &url_str[end..];
    }

    if !remaining.is_empty() {
        spans.push(Span::raw(remaining.to_string()));
    }

    spans
}

/// Process a single prose line into styled spans, handling `**bold**` markers
/// and `https://` URL linkification with OSC 8 sequences.
fn process_prose_line(line: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    if line.contains("**") {
        let parts: Vec<&str> = line.split("**").collect();
        for (i, part) in parts.iter().enumerate() {
            if i % 2 == 1 {
                // Odd parts are bold — no URL linkification inside bold text.
                spans.push(Span::styled(
                    part.to_string(),
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(Color::Yellow),
                ));
            } else {
                // Even parts are normal prose — linkify URLs, but always emit a
                // span (even empty) to preserve the span count tests rely on.
                let sub = linkify_text(part);
                if sub.is_empty() {
                    spans.push(Span::raw(part.to_string()));
                } else {
                    spans.extend(sub);
                }
            }
        }
    } else {
        let sub = linkify_text(line);
        if sub.is_empty() {
            spans.push(Span::raw(line.to_string()));
        } else {
            spans.extend(sub);
        }
    }

    spans
}

fn parse_simple_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;

    for raw_line in text.lines() {
        if raw_line.starts_with("```") {
            if in_code_block {
                // Closing fence — draw a bottom border.
                lines.push(Line::from(Span::styled(
                    "─────────────────────────────────────",
                    Style::default().fg(Color::DarkGray),
                )));
                in_code_block = false;
            } else {
                // Opening fence — extract the optional language tag.
                let lang = raw_line.trim_start_matches('`').trim();
                let label = if lang.is_empty() {
                    " ─── code ──────────────────────── ".to_string()
                } else {
                    format!(" ─── {} ──────────────────────── ", lang)
                };
                lines.push(Line::from(Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                in_code_block = true;
            }
        } else if in_code_block {
            // Inside a code block: preserve the raw content, no bold processing.
            lines.push(Line::from(Span::styled(
                raw_line.to_string(),
                Style::default().fg(Color::Green),
            )));
        } else {
            lines.push(Line::from(process_prose_line(raw_line)));
        }
    }

    // If the message ends inside an unclosed code block, close it.
    if in_code_block {
        lines.push(Line::from(Span::styled(
            "─────────────────────────────────────",
            Style::default().fg(Color::DarkGray),
        )));
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
                .map(|a| PlanStep {
                    agent: a.clone(),
                    task: "x".into(),
                    depends_on: None,
                })
                .collect(),
        }
    }

    #[test]
    fn route_conversational_shows_no_intent_suffix() {
        let s = format_route_decision(&plan(IntentType::Conversational, &[AgentKind::Chat]), None);
        assert_eq!(s, "Agent L → Chat");
    }

    #[test]
    fn route_factual_shows_factual_suffix() {
        let s = format_route_decision(&plan(IntentType::Factual, &[AgentKind::Search]), None);
        assert_eq!(s, "Agent L → Search (Factual)");
    }

    #[test]
    fn route_creative_shows_creative_suffix() {
        let s = format_route_decision(&plan(IntentType::Creative, &[AgentKind::Chat]), None);
        assert_eq!(s, "Agent L → Chat (Creative)");
    }

    #[test]
    fn route_task_multi_step_joins_with_plus() {
        let s = format_route_decision(
            &plan(IntentType::Task, &[AgentKind::Shell, AgentKind::Code]),
            None,
        );
        assert_eq!(s, "Agent L → Shell + Code");
    }

    #[test]
    fn route_deduplicates_repeated_agents() {
        // Two steps with the same agent should appear only once
        let s = format_route_decision(
            &plan(IntentType::Task, &[AgentKind::Chat, AgentKind::Chat]),
            None,
        );
        assert_eq!(s, "Agent L → Chat");
    }

    #[test]
    fn route_preserves_agent_order() {
        let s = format_route_decision(
            &plan(
                IntentType::Task,
                &[AgentKind::Search, AgentKind::Shell, AgentKind::Code],
            ),
            None,
        );
        assert_eq!(s, "Agent L → Search + Shell + Code");
    }

    #[test]
    fn route_code_with_one_off_scope() {
        let s = format_route_decision(
            &plan(IntentType::Task, &[AgentKind::Code]),
            Some(&TaskScope::OneOff),
        );
        assert_eq!(s, "Agent L → Code (one-off)");
    }

    #[test]
    fn route_code_with_project_scope() {
        let s = format_route_decision(
            &plan(IntentType::Task, &[AgentKind::Code]),
            Some(&TaskScope::Project),
        );
        assert_eq!(s, "Agent L → Code (project)");
    }

    #[test]
    fn route_code_without_scope_shows_plain_code() {
        let s = format_route_decision(&plan(IntentType::Task, &[AgentKind::Code]), None);
        assert_eq!(s, "Agent L → Code");
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

    // ── Fenced code block rendering ──────────────────────────────────────────

    /// A fenced code block with a language tag should produce:
    /// - one label line mentioning the language
    /// - one line per code line, styled differently from prose
    /// - one closing line
    #[test]
    fn code_block_with_language_produces_label_and_code_lines() {
        let text = "```rust\nlet x = 1;\n```";
        let lines = parse_simple_markdown(text);
        // 3 lines: label, code line, closing line
        assert_eq!(
            lines.len(),
            3,
            "expected label + 1 code line + closing, got {lines:?}"
        );
        // Label line mentions the language
        let label = lines[0].spans[0].content.to_string();
        assert!(
            label.contains("rust"),
            "label should mention language: {label:?}"
        );
        // Code line has distinct styling (not the default white)
        assert_ne!(
            lines[1].spans[0].style,
            Style::default(),
            "code line should be styled"
        );
        // Code line content is preserved exactly
        assert_eq!(lines[1].spans[0].content, "let x = 1;");
    }

    /// A fenced code block without a language tag should still render distinctly.
    #[test]
    fn code_block_without_language_produces_generic_label() {
        let text = "```\nhello\n```";
        let lines = parse_simple_markdown(text);
        assert_eq!(lines.len(), 3);
        let label = lines[0].spans[0].content.to_string();
        // Should still produce some kind of label, just without a language name
        assert!(!label.is_empty());
    }

    /// Prose before and after a code block should still render normally.
    #[test]
    fn prose_around_code_block_renders_correctly() {
        let text = "before\n```sh\necho hi\n```\nafter";
        let lines = parse_simple_markdown(text);
        // 5 lines: "before", label, "echo hi", closing, "after"
        assert_eq!(lines.len(), 5, "got {lines:?}");
        assert_eq!(lines[0].spans[0].content, "before");
        assert_eq!(lines[4].spans[0].content, "after");
    }

    /// Bold markers inside a code block should NOT be processed as markdown —
    /// the raw content should be preserved.
    #[test]
    fn bold_markers_inside_code_block_are_not_processed() {
        let text = "```\nthis **is** raw\n```";
        let lines = parse_simple_markdown(text);
        // label line + 1 code line + closing line
        assert_eq!(lines.len(), 3);
        // The code line should be a single raw span, not split on "**"
        assert_eq!(
            lines[1].spans.len(),
            1,
            "code line should not parse bold markers"
        );
        assert_eq!(lines[1].spans[0].content, "this **is** raw");
    }

    /// A code block that is never closed should still render — the whole
    /// remaining text is treated as code.
    #[test]
    fn unclosed_code_block_renders_without_panic() {
        let text = "```rust\nlet x = 1;";
        let lines = parse_simple_markdown(text);
        // label line + 1 code line (no closing — shouldn't panic)
        assert!(lines.len() >= 2, "expected at least label + code line");
    }

    /// Multiple code blocks in one message should each render correctly.
    #[test]
    fn multiple_code_blocks_both_render() {
        let text = "```py\nprint(1)\n```\nmiddle\n```js\nconsole.log(2)\n```";
        let lines = parse_simple_markdown(text);
        let combined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("py"), "first block language missing");
        assert!(combined.contains("print(1)"), "first block content missing");
        assert!(combined.contains("middle"), "prose between blocks missing");
        assert!(combined.contains("js"), "second block language missing");
        assert!(
            combined.contains("console.log(2)"),
            "second block content missing"
        );
    }

    // ── URL linkification ────────────────────────────────────────────────────

    /// A bare `https://` URL in prose should be rendered as `[source]` in
    /// underlined cyan — not as the raw URL text, which clutters the output.
    #[test]
    fn bare_url_gets_linkified() {
        let lines = parse_simple_markdown("Visit https://example.com for more info");
        let url_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "[source]")
            .expect("[source] span missing");
        assert!(
            url_span.style.add_modifier.contains(Modifier::UNDERLINED),
            "URL span should be underlined"
        );
        assert_eq!(
            url_span.style.fg,
            Some(Color::Cyan),
            "URL span should be cyan"
        );
        let all_content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(all_content.contains("Visit "));
        assert!(all_content.contains(" for more info"));
        // Raw URL must not appear in the rendered output
        assert!(
            !all_content.contains("https://"),
            "raw URL must not appear in rendered output"
        );
    }

    /// Non-URL text should not be affected by linkification.
    #[test]
    fn non_url_prose_unchanged() {
        let lines = parse_simple_markdown("No URLs here, just text.");
        let all_content: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(
            !all_content.contains('\x1b'),
            "no escape sequences in plain text"
        );
        assert_eq!(all_content, "No URLs here, just text.");
    }

    /// URLs inside bold markers should NOT be linkified (bold is already styled).
    #[test]
    fn url_inside_bold_not_linkified() {
        let lines = parse_simple_markdown("see **https://example.com** for details");
        let combined: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(combined.contains("https://example.com"));
        let bold_spans: Vec<_> = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .collect();
        assert!(!bold_spans.is_empty());
        assert!(
            !bold_spans[0].content.contains('\x1b'),
            "URL inside bold should not have escape sequences"
        );
    }

    /// "Source: https://..." should collapse to just "[source]" — the "Source: "
    /// label is redundant once the URL is already replaced with "[source]".
    #[test]
    fn source_prefix_is_stripped_with_url() {
        let lines = parse_simple_markdown(
            "Trump is president. Source: https://en.wikipedia.org/wiki/France",
        );
        let all_content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !all_content.contains("Source:"),
            "redundant 'Source:' label must be stripped, got: {all_content:?}"
        );
        assert!(
            all_content.contains("[source]"),
            "must still render [source], got: {all_content:?}"
        );
        assert!(
            !all_content.contains("https://"),
            "raw URL must not appear in rendered output"
        );
    }

    /// A URL at the end of a line should render as `[source]`, not the raw URL.
    #[test]
    fn url_at_end_of_line_linkified() {
        let lines = parse_simple_markdown("Answer here https://en.wikipedia.org/wiki/France");
        let url_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "[source]")
            .expect("[source] span missing");
        assert!(
            url_span.style.add_modifier.contains(Modifier::UNDERLINED),
            "URL at end of line should be underlined"
        );
        let all_content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !all_content.contains("https://"),
            "raw URL must not appear in rendered output"
        );
    }

    // ── status line: thinking token display ──────────────────────────────────

    fn render_status(app: &App) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(120, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(app, f.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        // Collect all cells from the last row (status line)
        let last_row = buf.area.height - 1;
        (0..buf.area.width)
            .map(|x| {
                buf.cell((x, last_row))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect::<String>()
            .trim()
            .to_string()
    }

    #[test]
    fn status_shows_think_count_when_nonzero() {
        let mut app = App::new_for_test();
        app.thinking_tokens = 42;
        let status = render_status(&app);
        assert!(
            status.contains("think"),
            "status should contain 'think' when thinking_tokens > 0, got: {status:?}"
        );
        assert!(
            status.contains("42"),
            "status should contain the thinking token count, got: {status:?}"
        );
    }

    #[test]
    fn status_hides_think_count_when_zero() {
        let app = App::new_for_test();
        let status = render_status(&app);
        assert!(
            !status.contains("think:"),
            "status should not show think count when thinking_tokens == 0, got: {status:?}"
        );
    }
}
