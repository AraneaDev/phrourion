use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Flex, Layout},
    prelude::Alignment,
    style::{Color, Style},
    text::Line,
    widgets::Paragraph,
};
use std::time::{Duration, Instant};

pub(super) const STARTUP_FRAMES: usize = 6;
const STARTUP_FRAME_MS: u64 = 900;
pub(super) const STARTUP_DURATION_MS: u64 = STARTUP_FRAME_MS * STARTUP_FRAMES as u64;
pub(super) const EXIT_FRAMES: usize = STARTUP_FRAMES;
pub(super) const SCENE_WIDTH: usize = 40;
pub(super) const SCENE_HEIGHT: usize = 18;

fn tower_scene(frame: usize, exiting: bool) -> String {
    let step = frame.min(5);
    let mut grid = vec![vec![' '; SCENE_WIDTH]; SCENE_HEIGHT];
    fn put(grid: &mut [Vec<char>], x: usize, y: usize, text: &str) {
        for (offset, ch) in text.chars().enumerate() {
            if let Some(cell) = grid.get_mut(y).and_then(|row| row.get_mut(x + offset)) {
                *cell = ch;
            }
        }
    }
    fn centered(grid: &mut [Vec<char>], y: usize, text: &str) {
        put(grid, (SCENE_WIDTH - text.len()) / 2, y, text);
    }
    centered(&mut grid, 0, "P H R O U R I O N");
    // Tower geometry never changes. All sprites use the same cell coordinates.
    for (y, row) in [
        "   /\\   ",
        "  /##\\  ",
        " /####\\ ",
        "|      |",
        "|      |",
        "|------|",
        "|      |",
        "|      |",
        "|      |",
        "|______|",
    ]
    .iter()
    .enumerate()
    {
        put(&mut grid, 16, y + 2, row);
    }
    let guard = if exiting {
        match step {
            0 => Some((18, 5)),
            1 => Some((18, 7)),
            2 => Some((24, 9)),
            3 => Some((29, 9)),
            4 => Some((34, 9)),
            _ => None,
        }
    } else {
        Some(match step {
            0 => (2, 9),
            1 => (8, 9),
            2 => (13, 9),
            3 => (18, 9),
            4 => (18, 7),
            _ => (18, 5),
        })
    };
    if let Some((x, y)) = guard {
        put(&mut grid, x, y, " o ");
        put(&mut grid, x, y + 1, "/|\\");
        put(
            &mut grid,
            x,
            y + 2,
            if step % 2 == 0 { "/ \\" } else { " /|" },
        );
    }
    let lit = (!exiting && step == 5) || (exiting && step == 0);
    if lit {
        put(&mut grid, 22, 5, "*");
    }
    let caption = match (exiting, step) {
        (true, 5) => "The tower is dark",
        (true, _) => "Leaving the watch",
        (false, 5) => "WATCH ESTABLISHED",
        (false, _) => "Taking the watch",
    };
    centered(&mut grid, 13, caption);
    centered(&mut grid, 15, "Aranea Development");
    centered(
        &mut grid,
        17,
        if exiting {
            "[ Space / q ] skip"
        } else {
            "[ Space ] skip"
        },
    );
    grid.into_iter()
        .map(|row| row.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn draw_animation(frame: &mut Frame, scene: String) {
    let vertical = Layout::vertical([Constraint::Length(SCENE_HEIGHT as u16)])
        .flex(Flex::Center)
        .split(frame.area())[0];
    let area = Layout::horizontal([Constraint::Length(SCENE_WIDTH as u16)])
        .flex(Flex::Center)
        .split(vertical)[0];
    let lines: Vec<Line> = scene
        .lines()
        .enumerate()
        .map(|(y, line)| {
            Line::styled(
                line.to_owned(),
                Style::default().fg(if y == 15 || y == 17 {
                    Color::DarkGray
                } else {
                    Color::Cyan
                }),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), area);
}

pub(super) fn startup_scene(frame: usize) -> String {
    tower_scene(frame, false)
}

pub(super) fn exit_scene(frame: usize) -> String {
    tower_scene(frame, true)
}

pub(super) fn startup_skips(key: KeyCode) -> bool {
    key == KeyCode::Char(' ')
}

pub(super) fn exit_skips(key: KeyCode) -> bool {
    matches!(key, KeyCode::Char('q') | KeyCode::Char(' '))
}

pub(super) async fn startup_screen(terminal: &mut DefaultTerminal) -> Result<()> {
    let started = Instant::now();
    let frame_time = Duration::from_millis(STARTUP_FRAME_MS);
    let duration = Duration::from_millis(STARTUP_DURATION_MS);
    terminal.clear()?;
    loop {
        if event::poll(Duration::from_millis(20))?
            && let Event::Key(key) = event::read()?
            && startup_skips(key.code)
        {
            break;
        }
        let elapsed = started.elapsed();
        if elapsed >= duration {
            break;
        }
        let frame = elapsed.as_millis() as usize / frame_time.as_millis() as usize;
        terminal.draw(|area| draw_animation(area, startup_scene(frame)))?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    terminal.clear()?;
    Ok(())
}

pub(super) async fn exit_screen(terminal: &mut DefaultTerminal) -> Result<()> {
    let started = Instant::now();
    let frame_time = Duration::from_millis(STARTUP_FRAME_MS);
    let duration = frame_time * EXIT_FRAMES as u32;
    terminal.clear()?;
    loop {
        if event::poll(Duration::from_millis(20))?
            && let Event::Key(key) = event::read()?
            && exit_skips(key.code)
        {
            break;
        }
        let elapsed = started.elapsed();
        if elapsed >= duration {
            break;
        }
        let frame = elapsed.as_millis() as usize / frame_time.as_millis() as usize;
        terminal.draw(|area| draw_animation(area, exit_scene(frame)))?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    terminal.clear()?;
    Ok(())
}
