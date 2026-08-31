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
mod config;
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
    /// File to open. Created on first save if it does not exist.
    file: Option<PathBuf>,

    /// Use this config file instead of the discovered one.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

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

    /// Where `--export` writes.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,

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

    // Exporting is a batch job: no terminal is acquired, so errors go to stderr
    // as an exit code rather than a status-line flash (SPEC.md §14.4).
    if let Some(src) = &cli.export {
        let format = export::Format::parse(&cli.format)
            .ok_or_else(|| anyhow::anyhow!("{:?} is not md, txt, html or pdf", cli.format))?;
        let page = export::html::Page {
            theme: render::theme::Theme::authored(&config.theme).unwrap_or_default(),
            measure: config.layout.measure,
            base: src.parent().unwrap_or(std::path::Path::new("")).to_path_buf(),
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
                export::Format::Text => print!("{}", export::plain::render(&flat)),
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
    let mut app = app::App::new(config, cli.file, explicit)?;
    // Applied through `App` rather than by editing the config, so a hot reload
    // cannot silently drop it.
    if cli.zen {
        app.set_zen(true);
    }

    let mut terminal = ratatui::init();
    use ratatui::crossterm::event::{
        DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
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
    let result = app.run_loop(&mut terminal);
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableFocusChange);
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    result
}
