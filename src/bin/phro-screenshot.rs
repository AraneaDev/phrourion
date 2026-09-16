use anyhow::{Context, Result};
use phrourion::{git, model::clean, registry, tui};
use ratatui::{Terminal, backend::TestBackend, buffer::Cell, style::Color};
use std::{env, fs, path::PathBuf};

fn color(value: Color) -> &'static str {
    match value {
        Color::Reset | Color::White => "#d8dee9",
        Color::Black => "#20252e",
        Color::Red => "#bf616a",
        Color::Green => "#a3be8c",
        Color::Yellow => "#ebcb8b",
        Color::Blue => "#81a1c1",
        Color::Magenta => "#b48ead",
        Color::Cyan => "#88c0d0",
        Color::Gray => "#7b88a1",
        Color::DarkGray => "#4c566a",
        Color::LightRed => "#d08770",
        Color::LightGreen => "#a3be8c",
        Color::LightYellow => "#ebcb8b",
        Color::LightBlue => "#81a1c1",
        Color::LightMagenta => "#b48ead",
        Color::LightCyan => "#8fbcbb",
        Color::Rgb(_, _, _) | Color::Indexed(_) => "#d8dee9",
    }
}

fn escape(cell: &Cell) -> String {
    clean(cell.symbol())
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let config = match args.next() {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => registry::config_path()?,
    };
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("docs/screenshots/dashboard.svg"));
    let mut app = tui::App::new(registry::load(&config)?.repos);
    for row in &mut app.rows {
        row.local = match git::snapshot(&row.repo).await {
            Ok(state) => phrourion::model::Observation::success(state),
            Err(error) => phrourion::model::Observation::failure(error),
        };
    }
    let backend = TestBackend::new(140, 42);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| tui::draw(frame, &app))?;
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize * 8;
    let height = buffer.area.height as usize * 16;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">"
    );
    svg.push_str("<rect width=\"100%\" height=\"100%\" fill=\"#20252e\"/>");
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            if cell.symbol() != " " {
                svg.push_str(&format!(
                    "<text x=\"{}\" y=\"{}\" fill=\"{}\" font-family=\"monospace\" font-size=\"14\">{}</text>",
                    x * 8,
                    y * 16 + 13,
                    color(cell.fg),
                    escape(cell)
                ));
            }
        }
    }
    svg.push_str("</svg>\n");
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
    }
    fs::write(&output, svg).with_context(|| format!("Cannot write {}", output.display()))?;
    println!("Wrote {}", output.display());
    Ok(())
}
