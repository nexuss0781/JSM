use indicatif::{ProgressBar, ProgressStyle};
use std::{io::IsTerminal, time::Duration};

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
/// Quiet and JSON modes suppress this channel so they stay machine-friendly;
/// terminals get a spinner and redirected stderr gets one stable status line.
pub struct OperationProgress {
    spinner: Option<ProgressBar>,
    mode: RenderMode,
}

impl OperationProgress {
    pub fn start(message: &str, quiet: bool, json: bool, non_interactive: bool) -> Self {
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
                }
            }
            RenderMode::StatusLine => {
                eprintln!("jsm: {message}");
                Self {
                    spinner: None,
                    mode,
                }
            }
        }
    }

    pub fn package_started(&mut self, message: &str) {
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
        if let Some(spinner) = &self.spinner {
            spinner.set_message(message.to_owned());
        }
    }
}

impl Drop for OperationProgress {
    fn drop(&mut self) {
        if let Some(spinner) = &self.spinner {
            spinner.finish_and_clear();
        }
    }
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
}
