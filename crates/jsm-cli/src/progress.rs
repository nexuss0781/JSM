use indicatif::{ProgressBar, ProgressStyle};
use std::{
    io::IsTerminal,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    Suppressed,
    Interactive,
    StatusLine,
}

fn render_mode(quiet: bool, json: bool, is_terminal: bool, non_interactive: bool) -> RenderMode {
    if quiet || json {
        RenderMode::Suppressed
    } else if is_terminal && !non_interactive {
        RenderMode::Interactive
    } else {
        RenderMode::StatusLine
    }
}

/// Uniform operation progress for interactive and scripted invocations.
///
/// Quiet and JSON modes suppress the progress channel unless the caller explicitly selects
/// `--progress=ndjson`; terminals get a spinner and redirected stderr gets status lines.
pub struct OperationProgress {
    spinner: Option<ProgressBar>,
    mode: RenderMode,
    ndjson: bool,
    last_event: Instant,
}

impl OperationProgress {
    pub fn start(message: &str, quiet: bool, json: bool, non_interactive: bool) -> Self {
        Self::start_with_ndjson(message, quiet, json, non_interactive, false)
    }

    pub fn start_with_ndjson(
        message: &str,
        quiet: bool,
        json: bool,
        non_interactive: bool,
        ndjson: bool,
    ) -> Self {
        if ndjson {
            emit_progress("operation-started", message);
            return Self {
                spinner: None,
                mode: RenderMode::Suppressed,
                ndjson: true,
                last_event: Instant::now(),
            };
        }
        let mode = render_mode(
            quiet,
            json,
            std::io::stderr().is_terminal(),
            non_interactive,
        );
        match mode {
            RenderMode::Suppressed => Self {
                spinner: None,
                mode,
                ndjson: false,
                last_event: Instant::now(),
            },
            RenderMode::Interactive => {
                let spinner = ProgressBar::new_spinner();
                spinner.set_style(
                    ProgressStyle::default_spinner().tick_strings(&["-", "\\", "|", "/"]),
                );
                spinner.set_message(message.to_owned());
                spinner.enable_steady_tick(Duration::from_millis(100));
                Self {
                    spinner: Some(spinner),
                    mode,
                    ndjson: false,
                    last_event: Instant::now(),
                }
            }
            RenderMode::StatusLine => {
                eprintln!("jsm: {message}");
                Self {
                    spinner: None,
                    mode,
                    ndjson: false,
                    last_event: Instant::now(),
                }
            }
        }
    }

    pub fn package_started(&mut self, message: &str) {
        if self.ndjson {
            emit_progress("package-started", message);
            self.last_event = Instant::now();
            return;
        }
        match self.mode {
            RenderMode::Suppressed => {}
            RenderMode::Interactive => {
                if let Some(spinner) = &self.spinner {
                    spinner.set_message(message.to_owned());
                }
            }
            RenderMode::StatusLine => eprintln!("jsm: {message}"),
        }
    }

    pub fn update_detail(&mut self, message: &str) {
        if self.ndjson {
            if self.last_event.elapsed() >= Duration::from_millis(100) {
                emit_progress("detail", message);
                self.last_event = Instant::now();
            }
        } else if let Some(spinner) = &self.spinner {
            spinner.set_message(message.to_owned());
        }
    }
}

impl Drop for OperationProgress {
    fn drop(&mut self) {
        if self.ndjson {
            emit_progress("operation-ended", "");
        }
        if let Some(spinner) = &self.spinner {
            spinner.finish_and_clear();
        }
    }
}

fn progress_event(event: &str, message: &str) -> serde_json::Value {
    serde_json::json!({"schema":"jsm.v1.progress","event":event,"message":message})
}

fn emit_progress(event: &str, message: &str) {
    println!("{}", progress_event(event, message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_and_non_interactive_modes_never_render_a_spinner() {
        assert_eq!(
            render_mode(false, false, true, true),
            RenderMode::StatusLine
        );
        assert_eq!(
            render_mode(false, false, false, false),
            RenderMode::StatusLine
        );
        assert_eq!(
            render_mode(false, true, true, false),
            RenderMode::Suppressed
        );
        assert_eq!(
            render_mode(true, false, true, false),
            RenderMode::Suppressed
        );
        assert_eq!(
            render_mode(false, false, true, false),
            RenderMode::Interactive
        );
    }

    #[test]
    fn progress_events_have_a_stable_schema() {
        let event = progress_event("package-started", "Fetching pkg@1.0.0");
        assert_eq!(event["schema"], "jsm.v1.progress");
        assert_eq!(event["event"], "package-started");
        assert_eq!(event["message"], "Fetching pkg@1.0.0");
    }
}
