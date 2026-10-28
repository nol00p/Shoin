//! Shoin — entry point.
//!
//! `ratatui::init()` acquires raw mode + the alternate screen AND installs a
//! panic hook that restores the terminal before printing the panic, so a crash
//! never leaves a wedged shell. `ratatui::restore()` undoes it. That is the
//! whole of build-order step 1.

// NOTE: there is deliberately NO crate-wide `allow(dead_code)` here.
// One stood while the build order was being worked through, and by the time it
// was finished it had accumulated a duplicate copy of the input grammar, six
// unreachable `Action` variants, and — worst — an `Action::ReplaceChar` the
// grammar emitted and nothing handled, so `r{c}` compiled and did nothing.
// The one field that genuinely leads its use (`VisualRow::editable`, the SPEC
// §14.5 transclusion seam) carries its own targeted allow.

mod app;
mod clipboard;
mod config;
mod diff;
mod export;
mod finder;
mod fs;
mod help;
mod image;
mod input;
mod render;
mod text;
mod transclude;
mod tree;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "shoin", version, about = "A no-clutter terminal Markdown editor")]
struct Cli {
    /// File to open. Created on first save if it does not exist. `-`, or no
    /// file with text piped in, reads stdin into an unnamed buffer.
    file: Option<PathBuf>,

    /// Use this config file instead of the discovered one.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Open read-only: every line concealed, edits refused, `q` quits.
    /// `i` after the refusal message, or `:view`, switches to editing.
    #[arg(long = "read-only", short = 'R')]
    read_only: bool,

    /// Start with every chrome affordance hidden.
    #[arg(long)]
    zen: bool,

    /// Print the resolved config and exit.
    #[arg(long)]
    print_config: bool,

    /// Write the shipped config into your config directory and exit. Refuses a
    /// directory that already holds one unless `--force` is given.
    #[arg(long)]
    init_config: bool,

    /// Let `--init-config` replace a config that is already there.
    #[arg(long)]
    force: bool,

    /// Export this file's finished text and exit, without starting the TUI.
    /// Writes `<name>.<format>` unless `--out` or `--stdout` says otherwise.
    #[arg(long, value_name = "FILE")]
    export: Option<PathBuf>,

    /// What `--export` writes: `md`, `txt`, `html` or `pdf`.
    #[arg(long, value_name = "FORMAT", default_value = "md")]
    format: String,

    /// Stretch every wrapped line but a paragraph's last to fill the measure.
    /// Only `--format html` and `--format txt` have a layout of their own to
    /// stretch.
    #[arg(long)]
    justify: bool,

    /// Where `--export` writes.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,

    /// Print this file as the editor shows it, styled, and exit — a reader's
    /// view without opening the editor. Plain text when stdout is not a
    /// terminal, or when `NO_COLOR` is set.
    /// `-`, or no FILE at all, reads stdin: `curl …/README.md | shoin -p`.
    #[arg(long, short = 'p', value_name = "FILE", num_args = 0..=1)]
    render: Option<Option<PathBuf>>,

    /// With `--render`, page the output through `$PAGER` (`less -R` when
    /// unset) when it is taller than the terminal.
    #[arg(long, requires = "render")]
    pager: bool,

    /// Print the exported document instead of writing it, so
    /// `shoin --export notes.md --stdout | pandoc …` stays a one-shot pipeline.
    /// Text formats only — a PDF is not something to print to a terminal.
    #[arg(long)]
    stdout: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let explicit = cli.config.clone();

    // BEFORE `config::load`, deliberately: a config that no longer parses is
    // exactly when someone reaches for `--init-config --force`, and loading
    // first would refuse the command that fixes the problem.
    if cli.init_config {
        return match &explicit {
            Some(dir) => config::init::seed(dir, cli.force).map(|done| {
                println!("wrote {} files to {}", done.written.len(), done.dir.display());
            }),
            None => config::init::run(cli.force),
        };
    }

    let config = config::load(explicit.as_deref())?;

    if cli.print_config {
        println!("{config:#?}");
        return Ok(());
    }

    if let Some(src) = &cli.render {
        use std::io::IsTerminal;
        // A bare `-p` at a prompt would sit waiting for typed input, which
        // nobody means. `-p -` still reads the terminal, as `cat -` does.
        if src.is_none() && std::io::stdin().is_terminal() {
            anyhow::bail!("--render needs a FILE, or text piped in");
        }
        let src = src.clone().unwrap_or_else(|| PathBuf::from("-"));
        let src = &src;
        let piped = if is_stdin(src) { Some(read_stdin()?) } else { None };
        if piped.is_none() && !src.is_file() {
            anyhow::bail!("{}: no such file", src.display());
        }
        let tty = std::io::stdout().is_terminal();
        let colour = tty && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
        let (width, height) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
        let paged = |text: &str| cli.pager && tty && text.lines().count() > height as usize;
        let text = if colour {
            let mut app = match &piped {
                Some(bytes) => {
                    let mut app = app::App::new(config, None, explicit)?;
                    app.open_unnamed(text::buffer::Buffer::unnamed(bytes.clone())?);
                    app
                }
                None => app::App::new(config, Some(src.clone()), explicit)?,
            };
            let mut text = render::print::styled(&mut app, width);
            if paged(&text) {
                // A pager passes colour through but not pixels: the escapes
                // would arrive as text. The boxes are drawn as outlines then.
                app.image_protocol = image::Protocol::None;
                text = render::print::styled(&mut app, width);
            }
            text
        } else {
            let flat = match &piped {
                Some(bytes) => {
                    let text = String::from_utf8(bytes.clone())
                        .map_err(|_| anyhow::anyhow!("stdin is not valid UTF-8"))?;
                    // Links resolve from the working directory, as they do in
                    // an unnamed buffer.
                    let here = std::env::current_dir()?.join("stdin.md");
                    transclude::compile::compile_text(&text, &here, &config.transclude)?
                }
                None => transclude::compile::compile(src, &config.transclude)?,
            };
            export::plain::render(&flat, config.layout.measure, false)
        };
        if !(paged(&text) && render::print::page(&text)) {
            print!("{text}");
        }
        return Ok(());
    }

    // Exporting is a batch job: no terminal is acquired, so errors go to stderr
    // as an exit code rather than a status-line flash (SPEC.md §14.4).
    if let Some(src) = &cli.export {
        let format = export::Format::parse(&cli.format)
            .ok_or_else(|| anyhow::anyhow!("{:?} is not md, txt, html or pdf", cli.format))?;
        if cli.justify && !matches!(format, export::Format::Html | export::Format::Text) {
            anyhow::bail!("--justify only applies to --format html or --format txt");
        }
        let page = export::html::Page {
            theme: render::theme::Theme::authored(&config.theme).unwrap_or_default(),
            measure: config.layout.measure,
            base: src.parent().unwrap_or(std::path::Path::new("")).to_path_buf(),
            justify: cli.justify,
        };
        let title = src
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".into());
        if cli.stdout {
            if format == export::Format::Pdf {
                anyhow::bail!("--stdout cannot print a PDF; give --out a path");
            }
            let flat = transclude::compile::compile(src, &config.transclude)?;
            match format {
                export::Format::Text => print!("{}", export::plain::render(&flat, page.measure, page.justify)),
                export::Format::Html => print!("{}", export::html::render(&flat, &title, &page)),
                _ => print!("{flat}"),
            }
            return Ok(());
        }
        let dest = cli
            .out
            .clone()
            .unwrap_or_else(|| export::default_path(src, format));
        export::write(src, &dest, format, &config.transclude, &page)?;
        eprintln!("{}", dest.display());
        return Ok(());
    }

    let mouse = config.input.mouse;
    // `shoin -`, or plain `shoin` at the end of a pipe. Read before the
    // terminal is set up: the editor then takes its keys from /dev/tty, which
    // crossterm opens by itself when stdin is not a terminal.
    use std::io::IsTerminal;
    let piped = match &cli.file {
        Some(f) if is_stdin(f) => true,
        None => !std::io::stdin().is_terminal(),
        Some(_) => false,
    };
    let mut app = if piped {
        let bytes = read_stdin()?;
        let mut app = app::App::new(config, None, explicit)?;
        app.open_unnamed(text::buffer::Buffer::unnamed(bytes)?);
        app
    } else {
        app::App::new(config, cli.file, explicit)?
    };
    // Applied through `App` rather than by editing the config, so a hot reload
    // cannot silently drop it.
    if cli.zen {
        app.set_zen(true);
    }
    if cli.read_only {
        app.set_view(true);
    }

    let mut terminal = ratatui::init();
    use ratatui::crossterm::event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture,
    };
    if mouse {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    }
    // Focus reporting is what makes `[editor] autoreload` feel immediate: the
    // moment you come back to the terminal is the moment the file is most
    // likely to have changed. Unconditional, unlike mouse capture — it costs
    // nothing, steals no terminal affordance from the user, and a terminal that
    // does not implement it simply never sends the event (the 1-second poll
    // covers that case).
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableFocusChange);
    // Bracketed paste makes a paste arrive as ONE event holding the text,
    // rather than as the keys it would take to type it. Typed, a paste goes
    // through auto-indent, list continuation and the `jk` escape alias, and a
    // paste into Normal mode runs as commands. Unconditional for the same
    // reason focus reporting is: a terminal without it just sends keys.
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let result = app.run_loop(&mut terminal);
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableFocusChange);
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    result
}

/// `-` is stdin, as it is for most tools that take a file.
fn is_stdin(path: &std::path::Path) -> bool {
    path.as_os_str() == "-"
}

fn read_stdin() -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    Ok(bytes)
}
