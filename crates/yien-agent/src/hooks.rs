//! Claude Code hooks (ADR-09): the per-pane `--settings` JSON that registers
//! them, and the decoder that turns a hook payload into a [`HookEvent`].
//!
//! Nothing here touches the disk; the caller writes the settings inside
//! YIEN's own state directory (RNF-14).

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::signal::{HookEvent, NotificationKind, SessionEndReason, SessionStartSource};

/// The environment variable that carries the per-pane hook token to
/// `yien-hook` (04 §2.4). The launcher sets it; the settings never hold it.
pub const HOOK_TOKEN_ENV: &str = "YIEN_HOOK_TOKEN";

/// The largest hook payload [`decode_claude_hook`] reads. Tool inputs (a
/// whole file for `Write`) make payloads large.
pub const MAX_HOOK_PAYLOAD: usize = 4 * 1024 * 1024;

/// Seconds Claude Code waits for `yien-hook` before going on without it.
pub const HOOK_TIMEOUT_SECS: u64 = 5;

/// A hook template a profile rule can reference (04 §4.7). Templates are
/// versioned code, not free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum HookTemplate {
    /// Claude Code `command` hooks calling `yien-hook <event>`.
    #[serde(rename = "claude-command-v1")]
    ClaudeCommandV1,
}

/// The Claude Code events the state machine uses, the only ones registered
/// (ADR-09).
pub const CLAUDE_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// The `Notification` types that mean "needs you"; the matcher keeps the
/// others from ever reaching YIEN.
const CLAUDE_NOTIFICATION_MATCHER: &str =
    "permission_prompt|elicitation_dialog|elicitation_url_dialog|agent_needs_input";

/// Why a hook could not be rendered or decoded. Never echoes payload content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HookError {
    /// The `yien-hook` path cannot be embedded in a shell command safely.
    #[error(
        "the yien-hook path is empty or contains a quote, $, backtick or control character"
    )]
    UnsafeHookPath,
    /// The payload exceeds [`MAX_HOOK_PAYLOAD`].
    #[error("hook payload of {len} bytes exceeds the {max} byte limit")]
    TooLarge {
        /// Payload size.
        len: usize,
        /// The limit.
        max: usize,
    },
    /// The payload is not the JSON object expected.
    #[error("malformed hook payload at line {line}, column {column}")]
    Malformed {
        /// 1-based line of the error.
        line: usize,
        /// 1-based column of the error.
        column: usize,
    },
    /// `hook_event_name` names an event YIEN does not register.
    #[error("hook payload names an event YIEN does not handle")]
    UnknownEvent,
    /// `session_id` is missing, too long or has characters outside
    /// `[A-Za-z0-9_-]`.
    #[error("hook payload carries an invalid session id")]
    InvalidSessionId,
}

/// Renders the JSON for `claude --settings <file>`: one `command` hook per
/// event in [`CLAUDE_HOOK_EVENTS`], each running `"<hook_exe>" <event>`.
///
/// `hook_exe` should use forward slashes: Claude Code may run hooks through
/// a POSIX shell, where a backslash inside double quotes can be eaten.
///
/// ```
/// let json = yien_agent::render_claude_settings("C:/YIEN/yien-hook.exe").unwrap();
/// assert!(json.contains(r#""command": "\"C:/YIEN/yien-hook.exe\" Stop""#));
/// assert!(yien_agent::render_claude_settings("bad\"path").is_err());
/// ```
pub fn render_claude_settings(hook_exe: &str) -> Result<String, HookError> {
    let unsafe_char = |c: char| c == '"' || c == '$' || c == '`' || c.is_control();
    if hook_exe.is_empty() || hook_exe.chars().any(unsafe_char) {
        return Err(HookError::UnsafeHookPath);
    }
    let mut hooks = Map::new();
    for event in CLAUDE_HOOK_EVENTS {
        let handler = json!({
            "type": "command",
            "command": format!("\"{hook_exe}\" {event}"),
            "timeout": HOOK_TIMEOUT_SECS,
        });
        let mut group = Map::new();
        if *event == "Notification" {
            group.insert("matcher".into(), CLAUDE_NOTIFICATION_MATCHER.into());
        }
        group.insert("hooks".into(), Value::Array(vec![handler]));
        hooks.insert((*event).into(), Value::Array(vec![Value::Object(group)]));
    }
    let settings = json!({ "hooks": hooks });
    Ok(format!("{settings:#}"))
}

#[derive(Deserialize)]
struct RawHook {
    hook_event_name: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    notification_type: Option<String>,
}

/// Decodes the JSON a Claude Code hook writes to stdin. Only the fields the
/// state machine needs are read; the rest (prompts, tool inputs) is skipped.
///
/// A payload that fails to decode should reach the machine as
/// [`crate::Signal::Ambiguous`].
///
/// ```
/// use yien_agent::{HookEvent, decode_claude_hook};
///
/// let payload = br#"{"hook_event_name":"Stop","session_id":"abc"}"#;
/// assert_eq!(decode_claude_hook(payload), Ok(HookEvent::Stop));
/// assert!(decode_claude_hook(b"not json").is_err());
/// ```
pub fn decode_claude_hook(payload: &[u8]) -> Result<HookEvent, HookError> {
    if payload.len() > MAX_HOOK_PAYLOAD {
        return Err(HookError::TooLarge {
            len: payload.len(),
            max: MAX_HOOK_PAYLOAD,
        });
    }
    let raw: RawHook = serde_json::from_slice(payload).map_err(|e| HookError::Malformed {
        line: e.line(),
        column: e.column(),
    })?;
    let event = match raw.hook_event_name.as_str() {
        "SessionStart" => HookEvent::SessionStart {
            session_id: Some(valid_session_id(raw.session_id)?),
            source: match raw.source.as_deref() {
                Some("startup") => SessionStartSource::Startup,
                Some("resume") => SessionStartSource::Resume,
                Some("clear") => SessionStartSource::Clear,
                Some("compact") => SessionStartSource::Compact,
                Some("fork") => SessionStartSource::Fork,
                _ => SessionStartSource::Other,
            },
        },
        "UserPromptSubmit" => HookEvent::UserPromptSubmit,
        "PreToolUse" => HookEvent::PreToolUse,
        "PostToolUse" => HookEvent::PostToolUse,
        "PermissionRequest" => HookEvent::PermissionRequest,
        "Notification" => HookEvent::Notification(match raw.notification_type.as_deref() {
            Some("permission_prompt") => NotificationKind::PermissionPrompt,
            Some("elicitation_dialog" | "elicitation_url_dialog") => NotificationKind::Elicitation,
            Some("agent_needs_input") => NotificationKind::AgentNeedsInput,
            _ => NotificationKind::Other,
        }),
        "Stop" => HookEvent::Stop,
        "StopFailure" => HookEvent::StopFailure,
        "SessionEnd" => HookEvent::SessionEnd(match raw.reason.as_deref() {
            Some("clear") => SessionEndReason::Clear,
            Some("resume") => SessionEndReason::Resume,
            Some("logout") => SessionEndReason::Logout,
            Some("prompt_input_exit") => SessionEndReason::PromptInputExit,
            _ => SessionEndReason::Other,
        }),
        _ => return Err(HookError::UnknownEvent),
    };
    Ok(event)
}

/// The id goes into `--resume <id>` (RN-05), so it is held to a strict shape.
fn valid_session_id(id: Option<String>) -> Result<String, HookError> {
    match id {
        Some(id)
            if !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
        {
            Ok(id)
        }
        _ => Err(HookError::InvalidSessionId),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN_SETTINGS: &str = r#"{
  "hooks": {
    "Notification": [
      {
        "hooks": [
          {
            "command": "\"D:/yien/yien-hook.exe\" Notification",
            "timeout": 5,
            "type": "command"
          }
        ],
        "matcher": "permission_prompt|elicitation_dialog|elicitation_url_dialog|agent_needs_input"
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          {
            "command": "\"D:/yien/yien-hook.exe\" PermissionRequest",
            "timeout": 5,
            "type": "command"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {
            "command": "\"D:/yien/yien-hook.exe\" PostToolUse",
            "timeout": 5,
            "type": "command"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "hooks": [
          {
            "command": "\"D:/yien/yien-hook.exe\" PreToolUse",
            "timeout": 5,
            "type": "command"
          }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {
            "command": "\"D:/yien/yien-hook.exe\" SessionEnd",
            "timeout": 5,
            "type": "command"
          }
        ]
      }
    ],
    "SessionStart": [
      {
        "hooks": [
          {
            "command": "\"D:/yien/yien-hook.exe\" SessionStart",
            "timeout": 5,
            "type": "command"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "command": "\"D:/yien/yien-hook.exe\" Stop",
            "timeout": 5,
            "type": "command"
          }
        ]
      }
    ],
    "StopFailure": [
      {
        "hooks": [
          {
            "command": "\"D:/yien/yien-hook.exe\" StopFailure",
            "timeout": 5,
            "type": "command"
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "command": "\"D:/yien/yien-hook.exe\" UserPromptSubmit",
            "timeout": 5,
            "type": "command"
          }
        ]
      }
    ]
  }
}"#;

    #[test]
    fn claude_settings_match_the_golden_json() {
        let json = render_claude_settings("D:/yien/yien-hook.exe").unwrap();
        assert_eq!(json, GOLDEN_SETTINGS);
    }

    #[test]
    fn claude_settings_register_exactly_the_events_the_machine_uses() {
        let json = render_claude_settings("yien-hook").unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        let mut events: Vec<&str> = value["hooks"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        events.sort_unstable();
        let mut expected = CLAUDE_HOOK_EVENTS.to_vec();
        expected.sort_unstable();
        assert_eq!(events, expected);
    }

    #[test]
    fn rnf_11_hook_token_never_appears_in_the_settings() {
        let json = render_claude_settings("yien-hook").unwrap();
        assert!(!json.contains(HOOK_TOKEN_ENV));
        assert!(!json.to_lowercase().contains("token"));
    }

    #[test]
    fn unsafe_hook_paths_are_refused() {
        for bad in ["", "a\"b", "$HOME/yien-hook", "a`b`", "a\nb"] {
            assert_eq!(
                render_claude_settings(bad),
                Err(HookError::UnsafeHookPath),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn rn_05_session_start_carries_the_exact_session_id() {
        let payload = br#"{"hook_event_name":"SessionStart","session_id":"3f2a-9c_1","source":"clear","transcript_path":"x"}"#;
        assert_eq!(
            decode_claude_hook(payload),
            Ok(HookEvent::SessionStart {
                session_id: Some("3f2a-9c_1".into()),
                source: SessionStartSource::Clear,
            })
        );
    }

    #[test]
    fn rn_05_unsafe_session_ids_are_refused() {
        let long = "a".repeat(129);
        for id in ["", "../x", "a b", "--resume", "x;rm", long.as_str()] {
            let payload = serde_json::json!({"hook_event_name": "SessionStart", "session_id": id});
            let bytes = serde_json::to_vec(&payload).unwrap();
            let expected = if id == "--resume" {
                // Dashes are allowed; the id is passed as its own argv item.
                Ok(())
            } else {
                Err(HookError::InvalidSessionId)
            };
            assert_eq!(decode_claude_hook(&bytes).map(|_| ()), expected, "{id:?}");
        }
        let missing = br#"{"hook_event_name":"SessionStart"}"#;
        assert_eq!(decode_claude_hook(missing), Err(HookError::InvalidSessionId));
    }

    #[test]
    fn every_registered_event_decodes() {
        for event in CLAUDE_HOOK_EVENTS {
            let payload = serde_json::json!({"hook_event_name": event, "session_id": "s1"});
            let bytes = serde_json::to_vec(&payload).unwrap();
            assert!(decode_claude_hook(&bytes).is_ok(), "{event}");
        }
    }

    #[test]
    fn notification_types_map_to_kinds() {
        let cases = [
            ("permission_prompt", NotificationKind::PermissionPrompt),
            ("elicitation_dialog", NotificationKind::Elicitation),
            ("elicitation_url_dialog", NotificationKind::Elicitation),
            ("agent_needs_input", NotificationKind::AgentNeedsInput),
            ("idle_prompt", NotificationKind::Other),
            ("elicitation_complete", NotificationKind::Other),
        ];
        for (ty, kind) in cases {
            let payload = serde_json::json!({"hook_event_name": "Notification", "notification_type": ty, "message": "m"});
            let bytes = serde_json::to_vec(&payload).unwrap();
            assert_eq!(
                decode_claude_hook(&bytes),
                Ok(HookEvent::Notification(kind))
            );
        }
    }

    #[test]
    fn session_end_reasons_map() {
        let cases = [
            ("clear", SessionEndReason::Clear),
            ("resume", SessionEndReason::Resume),
            ("logout", SessionEndReason::Logout),
            ("prompt_input_exit", SessionEndReason::PromptInputExit),
            ("other", SessionEndReason::Other),
            ("brand_new", SessionEndReason::Other),
        ];
        for (reason, expected) in cases {
            let payload = serde_json::json!({"hook_event_name": "SessionEnd", "reason": reason});
            let bytes = serde_json::to_vec(&payload).unwrap();
            assert_eq!(
                decode_claude_hook(&bytes),
                Ok(HookEvent::SessionEnd(expected))
            );
        }
    }

    #[test]
    fn bad_payloads_are_errors_that_do_not_echo_content() {
        let secret = "sk-secret-value";
        let cases: Vec<Vec<u8>> = vec![
            format!(r#"{{"hook_event_name": 7, "x": "{secret}"}}"#).into_bytes(),
            format!(r#"{{"hook_event_name":"{secret}"}}"#).into_bytes(),
            format!(r#"{{"hook_event_name":"Stop", "session_id": "{secret}"#).into_bytes(),
            b"[]".to_vec(),
            vec![0xff, 0xfe],
            Vec::new(),
        ];
        for payload in cases {
            let err = decode_claude_hook(&payload).unwrap_err();
            assert!(!err.to_string().contains(secret), "{err}");
        }
    }

    #[test]
    fn oversized_payloads_are_refused_before_parsing() {
        let payload = vec![b' '; MAX_HOOK_PAYLOAD + 1];
        assert!(matches!(
            decode_claude_hook(&payload),
            Err(HookError::TooLarge { .. })
        ));
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let alphabet = b"{}\":,[]Stop hook_event_name0aZ \\";
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        for len in 0..512 {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    alphabet[(seed % alphabet.len() as u64) as usize]
                })
                .collect();
            let _ = decode_claude_hook(&bytes);
        }
    }
}
