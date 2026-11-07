//! Logs panics through `tracing` before the default hook runs, so a release
//! build, which has no console, still leaves a trace in the log file.

use std::any::Any;
use std::panic::PanicHookInfo;

/// Longest panic message kept, in chars. Messages can quote runtime strings
/// (std's str-slicing panics do), so they are capped to limit what can leak.
const MAX_MESSAGE_CHARS: usize = 200;

/// Chains the logging hook in front of the current one.
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_owned());
        let message = message(info.payload());
        tracing::error!(%location, %message, "panic");
        previous(info);
    }));
}

fn message(payload: &(dyn Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    match text {
        Some(text) if text.chars().count() > MAX_MESSAGE_CHARS => {
            let mut cut: String = text.chars().take(MAX_MESSAGE_CHARS).collect();
            cut.push_str("...");
            cut
        }
        Some(text) => text.to_owned(),
        None => "<non-string payload>".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_and_string_payloads_are_kept() {
        assert_eq!(message(&"boom"), "boom");
        assert_eq!(message(&String::from("boom")), "boom");
    }

    #[test]
    fn other_payloads_are_not_logged() {
        assert_eq!(message(&42_u32), "<non-string payload>");
        assert_eq!(message(&vec![1_u8, 2, 3]), "<non-string payload>");
    }

    #[test]
    fn long_messages_are_capped_on_a_char_boundary() {
        let long = "é".repeat(MAX_MESSAGE_CHARS + 50);
        let cut = message(&long);
        assert_eq!(cut.chars().count(), MAX_MESSAGE_CHARS + 3);
        assert!(cut.ends_with("..."));
    }
}
