use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Paragraph, Widget, Wrap},
};
use crate::app::{App, Role};

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::default()
            .constraints([
                Constraint::Min(0),    // Chat History
                Constraint::Length(3), // Input Box
            ])
            .split(area);

        let mut list_items = Vec::new();

        for msg in &self.history {
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
            list_items.push(Line::from("")); // Spacer line
        }

        Paragraph::new(Text::from(list_items))
            .block(Block::bordered().title(" Chat History ").border_type(BorderType::Rounded))
            .wrap(Wrap { trim: true })
            .scroll((self.scroll_offset, 0))
            .render(chunks[0], buf);

        Paragraph::new(format!("> {}", self.input))
            .block(Block::bordered().title(" Prompt ").border_style(Style::default().fg(Color::Cyan)))
            .render(chunks[1], buf);

            let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(3)])
            .split(area);
        
                    // --- 1. PREPARE THE DATA ---
            let mut list_items = Vec::new();

            for msg in &self.history {
                // ... your role-based logic to push Spans/Lines into list_items ...
            }

            // --- 2. CAPTURE MEASUREMENTS FIRST (The "Systems Engineer" way) ---
            // We get our numbers while we still own the list_items Vec
            let total_lines = list_items.len();
            let viewport_height = chunks[0].height.saturating_sub(2); // Subtract borders

            // --- 3. SYNC WITH APP STATE ---
            // We use a small hack to update the App's knowledge of the UI
            // Since we are in a read-only render(&self), we need to update these
            // via the main loop or use interior mutability, but for now, 
            // let's just make sure we don't break ownership.
            let _ = total_lines; // Silences the unused warning

            // --- 4. CONSUME THE DATA ---
            // Now we move the data into the widget. It's gone after this line.
            Paragraph::new(Text::from(list_items))
                .block(Block::bordered().title(" Chat History "))
                .wrap(Wrap { trim: true })
                .scroll((self.scroll_offset, 0))
                .render(chunks[0], buf);
    }
}

// Added the <'_> to fix the lifetime warning
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