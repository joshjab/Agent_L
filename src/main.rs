mod app;
mod ollama;
mod ui;
mod config;

use std::io;
use crossterm::event::{self, Event, KeyCode};
use crate::app::App;

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new();

    while !app.exit {
        let total_lines = app.history.iter().fold(0, |acc, m| acc + m.content.lines().count() + 2); 

        app.enforce_auto_scroll(total_lines, app.terminal_height);

        terminal.draw(|f| f.render_widget(&app, f.area()))?;

        // Update state from background tasks
        app.update();

        // Handle Input (with a small timeout so the loop keeps spinning)
        if event::poll(std::time::Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                KeyCode::Char('q') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    app.exit = true;
                }
                KeyCode::Char(c) => {
                    app.input.push(c); 
                }
                KeyCode::Backspace => {
                    app.input.pop(); 
                }
                KeyCode::Up => {
                    app.auto_scroll = false;
                    app.scroll_offset = app.scroll_offset.saturating_sub(1);
                }
                KeyCode::Down => {
                    app.scroll_offset = app.scroll_offset.saturating_add(1);
                }
                KeyCode::Enter => {
                    app.ask_ollama();
                }
                _ => {}
            }
        }
        }
       
    }

    ratatui::restore();
    Ok(())
}