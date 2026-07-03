//! Desktop notifications: `osascript` on macOS, `notify-send` on Linux,
//! silently no-op elsewhere or if the platform binary is missing or fails.
//!
//! A bridge daemon must never crash — or even error — because a
//! notification couldn't be delivered, so every path here is best-effort.

/// Fire a best-effort desktop notification.
pub fn notify(title: &str, body: &str) {
    let title = strip_quotes(title);
    let body = strip_quotes(body);
    imp::notify(&title, &body);
}

/// Strip `"` characters from a string bound for an `osascript` literal.
/// Inputs here are strings tephra builds itself (vault names, remote
/// names), not adversarial input, so simple stripping — rather than full
/// shell/AppleScript escaping — is enough to keep the generated command
/// well-formed.
fn strip_quotes(s: &str) -> String {
    s.chars().filter(|&c| c != '"').collect()
}

#[cfg(target_os = "macos")]
mod imp {
    use std::process::Command;

    pub fn notify(title: &str, body: &str) {
        let script = format!(r#"display notification "{body}" with title "{title}""#);
        let _ = Command::new("osascript").arg("-e").arg(script).output();
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::process::Command;

    pub fn notify(title: &str, body: &str) {
        let _ = Command::new("notify-send").arg(title).arg(body).output();
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    pub fn notify(_title: &str, _body: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_quotes_removes_double_quotes() {
        assert_eq!(strip_quotes(r#"say "hi" now"#), "say hi now");
    }

    #[test]
    fn strip_quotes_leaves_other_punctuation_alone() {
        assert_eq!(
            strip_quotes("origin's unreachable; queuing"),
            "origin's unreachable; queuing"
        );
    }

    #[test]
    fn notify_never_panics_regardless_of_platform_binary_availability() {
        // Best-effort: we can't assert delivery in CI (no display / no
        // notifier binary), only that quoted, adversarial-looking input
        // never causes a panic.
        notify(
            "tephra-bridge \"personal\"",
            "origin unreachable; queuing \"locally\"",
        );
    }
}
