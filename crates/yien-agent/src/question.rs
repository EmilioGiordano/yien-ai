//! The text a Claude Code hook gives for what the agent is asking the user,
//! shown next to the pane in the attention tray (RF-ATN-13).
//!
//! Only the hook's own summary is read: a notification's `message`, or the
//! name of the tool a permission request is for. Tool inputs, prompts and
//! terminal content are never read.

use serde::Deserialize;

use crate::hooks::MAX_HOOK_PAYLOAD;

/// The longest question kept, in characters, the ellipsis included.
pub const MAX_QUESTION_CHARS: usize = 160;

/// The longest tool name kept, in characters.
const MAX_TOOL_CHARS: usize = 64;

#[derive(Deserialize)]
struct RawQuestion {
    hook_event_name: String,
    #[serde(default)]
    notification_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

/// What the agent asks the user, from a Claude Code hook payload: the
/// `message` of a `Notification` that asks for the user (a permission
/// prompt, an elicitation, input needed), or "Permission to use <tool>" for
/// a `PermissionRequest` of the main session. Put on one line, without
/// control or format characters, and cut to [`MAX_QUESTION_CHARS`]. `None`
/// for every other hook, a subagent's request, or a payload that does not
/// decode.
///
/// ```
/// use yien_agent::claude_hook_question;
///
/// let note = br#"{"hook_event_name":"Notification","notification_type":"permission_prompt",
///     "message":"Claude needs your permission to use Bash"}"#;
/// assert_eq!(claude_hook_question(note).as_deref(), Some("Claude needs your permission to use Bash"));
/// let ask = br#"{"hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{}}"#;
/// assert_eq!(claude_hook_question(ask).as_deref(), Some("Permission to use Edit"));
/// assert_eq!(claude_hook_question(br#"{"hook_event_name":"Stop"}"#), None);
/// ```
pub fn claude_hook_question(payload: &[u8]) -> Option<String> {
    if payload.len() > MAX_HOOK_PAYLOAD {
        return None;
    }
    let raw: RawQuestion = serde_json::from_slice(payload).ok()?;
    match raw.hook_event_name.as_str() {
        "Notification" => match raw.notification_type.as_deref() {
            Some(
                "permission_prompt"
                | "elicitation_dialog"
                | "elicitation_url_dialog"
                | "agent_needs_input",
            ) => one_line(raw.message.as_deref()?, MAX_QUESTION_CHARS),
            _ => None,
        },
        "PermissionRequest" if raw.agent_id.is_none() => {
            let tool = one_line(raw.tool_name.as_deref()?, MAX_TOOL_CHARS)?;
            Some(format!("Permission to use {tool}"))
        }
        _ => None,
    }
}

/// `text` on one line: runs of whitespace and control characters become one
/// space, format characters (bidi overrides, zero-width) are dropped, and
/// past `max` characters it is cut with an ellipsis.
fn one_line(text: &str, max: usize) -> Option<String> {
    let mut chars: Vec<char> = Vec::with_capacity(max + 1);
    let mut gap = false;
    for ch in text.chars() {
        if is_format(ch) {
            continue;
        }
        if ch.is_whitespace() || ch.is_control() {
            gap = !chars.is_empty();
            continue;
        }
        if gap {
            chars.push(' ');
            gap = false;
        }
        chars.push(ch);
        if chars.len() > max {
            chars.truncate(max - 1);
            while chars.last() == Some(&' ') {
                chars.pop();
            }
            chars.push('…');
            break;
        }
    }
    (!chars.is_empty()).then(|| chars.into_iter().collect())
}

/// Whether `ch` is a Unicode format character (general category Cf):
/// invisible, and able to reorder or hide the text around it.
fn is_format(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn question(payload: serde_json::Value) -> Option<String> {
        claude_hook_question(payload.to_string().as_bytes())
    }

    #[test]
    fn rf_atn_13_notifications_that_ask_for_the_user_carry_their_message() {
        for ty in [
            "permission_prompt",
            "elicitation_dialog",
            "elicitation_url_dialog",
            "agent_needs_input",
        ] {
            let payload = json!({
                "hook_event_name": "Notification",
                "notification_type": ty,
                "message": "Claude needs your permission to use Bash",
            });
            assert_eq!(
                question(payload).as_deref(),
                Some("Claude needs your permission to use Bash"),
                "{ty}"
            );
        }
    }

    #[test]
    fn rf_atn_13_other_notifications_and_hooks_carry_nothing() {
        let idle = json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt",
            "message": "Claude is waiting for your input",
        });
        assert_eq!(question(idle), None);
        let untyped = json!({"hook_event_name": "Notification", "message": "m"});
        assert_eq!(question(untyped), None);
        let no_message = json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
        });
        assert_eq!(question(no_message), None);
        for event in ["Stop", "StopFailure", "UserPromptSubmit", "PreToolUse"] {
            let payload = json!({"hook_event_name": event, "message": "m", "tool_name": "Bash"});
            assert_eq!(question(payload), None, "{event}");
        }
        assert_eq!(claude_hook_question(b"not json"), None);
    }

    #[test]
    fn rf_atn_13_a_permission_request_names_its_tool_but_never_its_input() {
        let payload = json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf secret"},
        });
        let text = question(payload).unwrap();
        assert_eq!(text, "Permission to use Bash");
        assert!(!text.contains("secret"));
        let no_tool = json!({"hook_event_name": "PermissionRequest"});
        assert_eq!(question(no_tool), None);
    }

    #[test]
    fn rn_02_a_subagent_permission_request_carries_nothing() {
        let payload = json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "agent_id": "sub-1",
        });
        assert_eq!(question(payload), None);
    }

    #[test]
    fn rf_atn_13_the_question_is_one_bounded_line_without_hidden_characters() {
        let payload = json!({
            "hook_event_name": "Notification",
            "notification_type": "agent_needs_input",
            "message": "  Pick\none\u{202E}\u{200B}\tof these\u{1b}[31m  ",
        });
        assert_eq!(question(payload).as_deref(), Some("Pick one of these [31m"));
        let long = json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": "word ".repeat(100),
        });
        let text = question(long).unwrap();
        assert_eq!(text.chars().count(), MAX_QUESTION_CHARS);
        assert!(text.ends_with("word…") || text.ends_with('…'));
        assert!(!text.contains(" …"));
        let blank = json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": " \u{200B}\n ",
        });
        assert_eq!(question(blank), None);
        let tool = json!({"hook_event_name": "PermissionRequest", "tool_name": "x".repeat(200)});
        let text = question(tool).unwrap();
        assert_eq!(
            text.chars().count(),
            "Permission to use ".len() + MAX_TOOL_CHARS
        );
    }

    #[test]
    fn an_oversized_payload_carries_nothing() {
        let mut payload =
            br#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","x":""#.to_vec();
        payload.resize(MAX_HOOK_PAYLOAD + 1, b'a');
        assert_eq!(claude_hook_question(&payload), None);
    }
}
