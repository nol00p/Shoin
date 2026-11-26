//! Shoin — entry point.
//!
//! `ratatui::init()` acquires raw mode + the alternate screen AND installs a
//! panic hook that restores the terminal before printing the panic, so a crash
//! never leaves a wedged shell. `recover::restore_terminal()` undoes it, and
//! replaces that hook with one of its own (see `main`).

// NOTE: there is deliberately NO crate-wide `allow(dead_code)` here.
// One stood while the build order was being worked through, and by the time it
// was finished it had accumulated a duplicate copy of the input grammar, six
// unreachable `Action` variants, and — worst — an `Action::ReplaceChar` the
// grammar emitted and nothing handled, so `r{c}` compiled and did nothing.
// `VisualRow::editable` has a targeted allow because it is reserved for
// editable transclusions.

mod app;
mod clipboard;
mod config;
mod diff;
#[cfg(test)]
mod docs_sync;
mod export;
mod finder;
mod fs;
mod help;
mod image;
mod input;
mod recover;
mod render;
#[cfg(test)]
mod test_support;
mod text;
mod transclude;
mod tree;
mod values;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

/// The `--format` line of `--help`: "What `--export` writes: `md`, `txt`,
/// `html` or `pdf`.", from `values::EXPORT_VALUES`.
fn format_help() -> String {
    let formats: Vec<String> = values::EXPORT_VALUES.iter().map(|f| format!("`{f}`")).collect();
    format!("What `--export` writes: {}.", values::or_list(&formats))
}

/// `--format` read, and `--justify` checked against it.
fn export_format(name: &str, justify: bool) -> anyhow::Result<export::Format> {
    let format = export::Format::parse(name)
        .ok_or_else(|| anyhow::anyhow!("{name:?} is not {}", values::or_list(values::EXPORT_VALUES)))?;
    if justify && !values::export_justifies(format) {
        let formats: Vec<String> = values::justify_formats().iter().map(|f| format!("--format {f}")).collect();
        anyhow::bail!("--justify only applies to {}", values::or_list(&formats));
    }
    Ok(format)
}

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
    /// `:view` switches to editing at any time, and so does `i` as the first
    /// key or right after a refusal.
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

    /// What `--export` writes (`format_help`).
    #[arg(long, value_name = "FORMAT", default_value = "md", help = format_help())]
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
    #[cfg(unix)]
    if cli.render.is_some() || cli.export.is_some() {
        watch_cli_signals()?;
    }

    if cli.print_config {
        println!("{config:#?}");
        return Ok(());
    }

    if let Some(src) = &cli.render {
        use std::io::IsTerminal;
        // `-p`'s file is optional, so in `shoin -p --pager notes.md` (or
        // `shoin notes.md -p`) clap hands the file to FILE instead. It is the
        // same file either way.
        let src = src.clone().or_else(|| cli.file.clone());
        // A bare `-p` at a prompt would sit waiting for typed input, which
        // nobody means. `-p -` still reads the terminal, as `cat -` does.
        if src.is_none() && std::io::stdin().is_terminal() {
            anyhow::bail!("--render needs a FILE, or text piped in");
        }
        let src = src.unwrap_or_else(|| PathBuf::from("-"));
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
            export::plain::render(&flat, config.layout.measure, false, &config.markdown.for_export())
        };
        if !(paged(&text) && render::print::page(&text)) {
            print_out(&text)?;
        }
        return Ok(());
    }

    // Batch export skips terminal setup, so errors go to stderr and the
    // command exits with a failure status.
    if let Some(src) = &cli.export {
        for warning in config::validate(&config) {
            eprintln!("config: {warning}");
        }
        let format = export_format(&cli.format, cli.justify)?;
        let page = export::html::Page {
            theme: render::theme::Theme::authored(&config.theme).unwrap_or_default(),
            measure: config.layout.measure,
            base: src.parent().unwrap_or(std::path::Path::new("")).to_path_buf(),
            justify: cli.justify,
            md: config.markdown.for_export(),
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
                export::Format::Text => print_out(&export::plain::render(&flat, page.measure, page.justify, &page.md))?,
                export::Format::Html => {
                    let (html, note) = export::html::render_with_mermaid(&flat, &title, &page, Default::default());
                    if let Some(note) = note {
                        eprintln!("{note}");
                    }
                    print_out(&html)?
                }
                _ => print_out(&flat)?,
            }
            return Ok(());
        }
        let dest = cli
            .out
            .clone()
            .unwrap_or_else(|| export::default_path(src, format));
        // Export diagnostics go to stderr with the path, so
        // the export still composes in a script.
        let note = export::write(src, &dest, format, &config.transclude, &page)?;
        eprintln!("{}", dest.display());
        if let Some(note) = note {
            eprintln!("{note}");
        }
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
        let startup = app.flash.take();
        app.set_view(true);
        app.flash = startup.or(app.flash);
    }

    // The standard hook, which prints the panic message, taken before
    // `ratatui::init` wraps it in one that restores the terminal: ours restores
    // by itself, and `ratatui::restore` can panic (see `recover::restore_terminal`).
    let print_panic = std::panic::take_hook();
    let mut terminal = ratatui::init();
    use ratatui::crossterm::event::{
        EnableBracketedPaste, EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
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
    // The kitty keyboard protocol reports Ctrl/Alt chords as one sequence, so
    // `Alt-h` cannot split into Esc + `h` (over SSH, in tmux) and `Ctrl-h` is
    // not Backspace. Alternate keys keep the case of a Shift chord (`Alt-J`).
    // A terminal without the protocol ignores the request and sends legacy
    // keys; `input::legacy` folds the keys that differ back to their old
    // meaning either way.
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    );
    // `ratatui::init`'s panic hook restores raw mode and the screen but knows
    // nothing of the modes above, and its restore can itself panic on a dead
    // terminal. It is replaced: `recover::restore_terminal` undoes everything,
    // and `print_panic` prints the message once the terminal is back.
    //
    // The unsaved text goes to recovery files FIRST: it is the one thing a
    // crash can lose for good, and nothing after it may get the chance.
    drop(std::panic::take_hook());
    std::panic::set_hook(Box::new(move |info| {
        // Recovery files are written once, by whoever ends the editor first;
        // `recover::OnPanic` spells out each case.
        match recover::on_panic() {
            recover::OnPanic::SafetyCopy { dump } => {
                if dump {
                    recover::dump();
                }
                recover::note(format!(
                    "a background thread failed; the editor kept running and this is not a crash: {info}"
                ));
                return;
            }
            recover::OnPanic::SignalThread => {
                recover::settle();
                recover::restore_terminal();
                print_panic(info);
                recover::report();
                finish_external_work();
                std::process::exit(recover::signal_status());
            }
            recover::OnPanic::Wait => {
                recover::note(info.to_string());
                recover::abandon_frame();
                loop {
                    std::thread::park();
                }
            }
            recover::OnPanic::Owner => {
                recover::dump();
                recover::settle();
            }
            recover::OnPanic::Restore => {}
        }
        recover::restore_terminal();
        print_panic(info);
        recover::report();
    }));
    // SIGTERM and SIGHUP (the window closed) would end the process where it
    // stands, terminal modes and unsaved text with it. Caught, they end it the
    // way a crash does, from a thread of their own. Frames stop before the
    // dump, and the terminal is restored only once the frame being drawn is
    // done, so no part of one lands on the shell.
    recover::catch_signals(|sig| {
        if !recover::stop(sig) {
            // The loop's exit or a panic owns the ending and writes the
            // recovery files; this thread only makes sure the process ends.
            recover::wait_for_owner();
        }
        recover::dump();
        recover::settle();
        recover::restore_terminal();
        recover::report();
        finish_external_work();
        std::process::exit(recover::signal_status());
    });
    let ended = app.run_loop(&mut terminal);
    // A signal or a panic that came first ends the editor from its own
    // thread: restoring here too would do it twice, and race its writes.
    if matches!(ended, Ok(recover::Ended::Stopped)) || !recover::quit() {
        loop {
            std::thread::park();
        }
    }
    // An error that ends the loop (a terminal that went away) leaves the
    // buffers unsaved: the editor is dying, so the text is kept.
    if ended.is_err() {
        recover::dump();
    }
    recover::restore_terminal();
    recover::report();

    finish_external_work();
    ended.map(|_| ())
}

fn finish_external_work() {
    export::shutdown();
}

#[cfg(unix)]
fn watch_cli_signals() -> Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

    let mut signals = signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM])?;
    std::thread::Builder::new().name("export-signals".into()).spawn(move || {
        if let Some(signal) = signals.forever().next() {
            finish_external_work();
            std::process::exit(128 + signal);
        }
    })?;
    Ok(())
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

/// Write to stdout for a pipeline. A reader that stops early (`| head`) closes
/// the pipe, and that is the reader's choice, not an error: `print!` would
/// panic on it.
fn print_out(text: &str) -> Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--help`, `--format` and `--justify` name the formats `values` lists.
    #[test]
    fn format_messages_read_the_value_list() {
        assert_eq!(format_help(), "What `--export` writes: `md`, `txt`, `html` or `pdf`.");
        let err = export_format("docx", false).unwrap_err().to_string();
        assert_eq!(err, format!("\"docx\" is not {}", values::or_list(values::EXPORT_VALUES)));
        assert_eq!(err, "\"docx\" is not md, txt, html or pdf");
        let err = export_format("md", true).unwrap_err().to_string();
        assert_eq!(err, "--justify only applies to --format txt or --format html");
        assert!(export_format("html", true).is_ok());
    }
}
