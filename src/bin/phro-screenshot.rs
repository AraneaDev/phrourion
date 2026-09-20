use anyhow::{Context, Result, bail};
use phrourion::{
    git::{self, LocalState},
    model::{Item, Observation, ProviderKind, Remote, RemoteState, Repo, clean},
    registry, tui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Cell, style::Color};
use serde::Deserialize;
use std::{env, fs, path::PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScreenshotMode {
    Dashboard,
    Help,
}

impl ScreenshotMode {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "dashboard" => Ok(Self::Dashboard),
            "help" => Ok(Self::Help),
            other => anyhow::bail!("unknown screenshot mode: {other}"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ScreenshotFixture {
    dashboard: Vec<ScreenshotFrame>,
    help: Vec<ScreenshotFrame>,
}

#[derive(Debug, Deserialize)]
struct ScreenshotFrame {
    label: String,
    repos: Vec<ScreenshotRepo>,
}

#[derive(Debug, Deserialize)]
struct ScreenshotRepo {
    name: String,
    local: Option<ScreenshotLocalState>,
    remote: ScreenshotRemoteState,
}

#[derive(Debug, Deserialize)]
struct ScreenshotLocalState {
    branch: String,
    head: String,
    #[serde(default)]
    upstream: String,
    #[serde(default)]
    ahead: usize,
    #[serde(default)]
    behind: usize,
    #[serde(default)]
    modified: usize,
    #[serde(default)]
    untracked: usize,
    #[serde(default)]
    conflicts: usize,
}

#[derive(Debug, Deserialize)]
struct ScreenshotRemoteState {
    #[serde(default)]
    prs: usize,
    #[serde(default)]
    review_requests: usize,
    #[serde(default)]
    issues: usize,
    #[serde(default)]
    proposals: usize,
    #[serde(default)]
    drafts: usize,
    ci: String,
}

impl ScreenshotFixture {
    fn frame(&self, mode: ScreenshotMode, index: usize) -> Result<&ScreenshotFrame> {
        let frames = match mode {
            ScreenshotMode::Dashboard => &self.dashboard,
            ScreenshotMode::Help => &self.help,
        };
        frames.get(index).with_context(|| {
            format!(
                "Screenshot frame {index} is out of range for {} ({} frames)",
                match mode {
                    ScreenshotMode::Dashboard => "dashboard",
                    ScreenshotMode::Help => "help",
                },
                frames.len()
            )
        })
    }
}

impl ScreenshotFrame {
    fn app(&self) -> tui::App {
        let _frame_label = &self.label;
        let repos = self
            .repos
            .iter()
            .map(|repo| Repo {
                id: format!("fixture-{}", repo.name),
                name: repo.name.clone(),
                path: PathBuf::from(format!("/fixtures/{}", repo.name)),
                remote: "origin".into(),
                identity: Remote {
                    kind: ProviderKind::Github,
                    host: "github.com".into(),
                    project: format!("AraneaDev/{}", repo.name),
                },
                enabled: true,
                release_workflows: Vec::new(),
                release_labels: Vec::new(),
                issue_labels: Vec::new(),
                workspaces: Vec::new(),
            })
            .collect();
        let mut app = tui::App::with_registry(registry::Registry {
            repos,
            ..registry::Registry::default()
        });
        for (row, fixture_repo) in app.rows.iter_mut().zip(&self.repos) {
            row.local = fixture_repo
                .local
                .as_ref()
                .map(|state| Observation::success(state.local_state()))
                .unwrap_or_default();
            row.remote = fixture_repo.remote.remote_state();
        }
        app
    }
}

impl ScreenshotLocalState {
    fn local_state(&self) -> LocalState {
        LocalState {
            branch: self.branch.clone(),
            head: self.head.clone(),
            upstream: self.upstream.clone(),
            ahead: self.ahead,
            behind: self.behind,
            modified: self.modified,
            untracked: self.untracked,
            conflicts: self.conflicts,
            ..LocalState::default()
        }
    }
}

impl ScreenshotRemoteState {
    fn remote_state(&self) -> RemoteState {
        let observation = |count: usize, prefix: &str| {
            Observation::success(
                (0..count)
                    .map(|index| Item {
                        title: format!("{prefix} {index}"),
                        detail: "fixture".into(),
                        url: "https://example.invalid/fixture".into(),
                    })
                    .collect(),
            )
        };
        let ci = match self.ci.as_str() {
            "loading" => Observation::default(),
            "fail" => Observation::success(vec![Item {
                title: "checks".into(),
                detail: "failure".into(),
                url: "https://example.invalid/fixture/checks".into(),
            }]),
            _ => Observation::success(vec![Item {
                title: "checks".into(),
                detail: "success".into(),
                url: "https://example.invalid/fixture/checks".into(),
            }]),
        };
        RemoteState {
            default_branch: if self.ci == "loading" {
                Observation::default()
            } else {
                Observation::success("main".into())
            },
            branches: observation(0, "branch"),
            prs: observation(self.prs, "PR"),
            review_requests: observation(self.review_requests, "review"),
            issues: observation(self.issues, "issue"),
            proposals: observation(self.proposals, "release"),
            drafts: observation(self.drafts, "draft"),
            published: observation(0, "published"),
            ci,
            publication: observation(0, "publication"),
        }
    }
}

fn load_fixture() -> Result<ScreenshotFixture> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/screenshots/frames.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("Cannot read screenshot fixture {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("Cannot parse screenshot fixture {}", path.display()))
}

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

fn render_svg(app: &mut tui::App) -> Result<String> {
    let backend = TestBackend::new(140, 42);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| tui::draw(frame, app))?;
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
    Ok(svg)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let fixture_mode = args.first().map(String::as_str) == Some("--fixture");
    let (output, mut app, live) = if fixture_mode {
        if args.len() != 5 || args[2] != "--frame" {
            bail!("Usage: --fixture <dashboard|help> --frame <index> <output>");
        }
        let mode = ScreenshotMode::parse(&args[1])?;
        let frame_index: usize = args[3].parse().context("Frame index must be an integer")?;
        let fixture = load_fixture()?;
        let mut app = fixture.frame(mode, frame_index)?.app();
        if mode == ScreenshotMode::Help {
            app.open_help();
        }
        (PathBuf::from(&args[4]), app, false)
    } else {
        let mut positional = args.into_iter();
        let config = match positional.next() {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => registry::config_path()?,
        };
        let output = positional
            .next()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("docs/screenshots/dashboard.svg"));
        let mode = ScreenshotMode::parse(positional.next().as_deref().unwrap_or("dashboard"))?;
        let mut app = tui::App::with_registry(registry::load(&config)?);
        if mode == ScreenshotMode::Help {
            app.open_help();
        }
        (
            output,
            app,
            env::var_os("PHROURION_SCREENSHOT_LIVE").is_some(),
        )
    };
    if live {
        for row in &mut app.rows {
            row.local = match git::snapshot(&row.repo).await {
                Ok(state) => Observation::success(state),
                Err(error) => Observation::failure(error),
            };
        }
    } else if !fixture_mode {
        for row in &mut app.rows {
            row.local = Observation::success(LocalState {
                branch: "main".into(),
                head: "0000000".into(),
                upstream: "origin/main".into(),
                ..LocalState::default()
            });
        }
    }
    let svg = render_svg(&mut app)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
    }
    fs::write(&output, svg).with_context(|| format!("Cannot write {}", output.display()))?;
    println!("Wrote {}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ScreenshotMode, load_fixture};
    use phrourion::tui::draw;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn fixture_has_four_dashboard_states_in_order() {
        let frames = load_fixture().unwrap().dashboard;
        assert_eq!(frames.len(), 4);
        assert_eq!(frames[0].label, "loading");
        assert_eq!(frames[1].label, "healthy");
        assert_eq!(frames[2].label, "attention");
        assert_eq!(frames[3].label, "recovered");
    }

    #[test]
    fn fixture_rejects_an_out_of_range_frame() {
        let fixture = load_fixture().unwrap();
        assert!(fixture.frame(ScreenshotMode::Dashboard, 4).is_err());
    }

    #[test]
    fn renders_fixture_frames_with_version_and_state_progression() {
        let fixture = load_fixture().unwrap();
        let render = |index| {
            let mut app = fixture
                .frame(ScreenshotMode::Dashboard, index)
                .unwrap()
                .app();
            let mut terminal = Terminal::new(TestBackend::new(140, 42)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };

        let loading = render(0);
        let healthy = render(1);
        let attention = render(2);
        let recovered = render(3);
        assert!(loading.contains(concat!("P H R O U R I O N  v", env!("CARGO_PKG_VERSION"))));
        assert!(loading.contains("loading"));
        assert!(healthy.contains("ACTION: all clear"));
        assert!(attention.contains("CI failed"));
        assert!(recovered.contains("ACTION: all clear"));
    }

    #[test]
    fn screenshot_mode_accepts_dashboard_and_help() {
        assert_eq!(
            ScreenshotMode::parse("dashboard").unwrap(),
            ScreenshotMode::Dashboard
        );
        assert_eq!(ScreenshotMode::parse("help").unwrap(), ScreenshotMode::Help);
        assert!(ScreenshotMode::parse("unknown").is_err());
    }
}
