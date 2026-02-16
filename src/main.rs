mod app;
mod ollama;
mod ui;

use std::io;
use crossterm::event::{self, Event, KeyCode};
use crate::app::App;

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new();

    while !app.exit {
        // 1. Draw
        terminal.draw(|f| f.render_widget(&app, f.area()))?;

        // 2. Update state from background tasks
        app.update();

        // 3. Handle Input (with a small timeout so the loop keeps spinning)
        if event::poll(std::time::Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                KeyCode::Char('q') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                    app.exit = true;
                }
                KeyCode::Char(c) => {
                    app.input.push(c); // Add typed char to our buffer
                }
                KeyCode::Backspace => {
                    app.input.pop(); // Remove last char
                }
                KeyCode::Up => {
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