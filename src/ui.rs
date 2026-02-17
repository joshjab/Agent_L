use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect, Alignment},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Paragraph, Widget, Wrap},
};
use crate::app::{App, Role};

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::default()
            .constraints([
                Constraint::Min(0),
                Constraint::Length(3),
                Constraint::Length(1), 
            ])
            .split(area);
        
        let mut list_items = Vec::new();
        let chat_width = chunks[0].width.saturating_sub(4); // Inner width for the separator

        for msg in &self.history {
            // Horizontal separator line before each new message
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
            // Add a small gap after the message
            list_items.push(Line::from("")); 
        }

        //Main Block
        Paragraph::new(Text::from(list_items))
            .block(
                Block::bordered()
                    .title(
                        Line::from(" 🦙 Agent L ")
                            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
                    )
                    .title_alignment(Alignment::Center)
                    .border_type(BorderType::Rounded)
            )
            .wrap(Wrap { trim: true })
            .scroll((self.scroll_offset, 0))
            .render(chunks[0], buf);

        //Prompt Block
        Paragraph::new(format!("> {}", self.input))
            .block(Block::bordered().title(" Prompt ").border_style(Style::default().fg(Color::Cyan)))
            .render(chunks[1], buf);

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