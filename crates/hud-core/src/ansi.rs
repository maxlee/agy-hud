pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const ITALIC: &str = "\x1b[3m";

pub mod colors {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const GRAY: &str = "\x1b[90m";
    pub const RED: &str = "\x1b[31m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const BLUE: &str = "\x1b[34m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const CYAN: &str = "\x1b[36m";
    pub const WHITE: &str = "\x1b[37m";
    pub const BRIGHT_RED: &str = "\x1b[91m";
    pub const BRIGHT_GREEN: &str = "\x1b[92m";
    pub const BRIGHT_YELLOW: &str = "\x1b[93m";
    pub const BRIGHT_BLUE: &str = "\x1b[94m";
    pub const BRIGHT_MAGENTA: &str = "\x1b[95m";
    pub const BRIGHT_CYAN: &str = "\x1b[96m";
    pub const BRIGHT_WHITE: &str = "\x1b[97m";
}

fn is_unsafe_control_or_bidi(ch: char) -> bool {
    let cp = ch as u32;
    cp <= 0x1f
        || (0x7f..=0x9f).contains(&cp)
        || matches!(
            cp,
            0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
        )
}

/// Scans an ANSI escape sequence starting at byte index `start` (where `bytes[start] == 0x1b`).
/// If `consume_unterminated` is true, unterminated OSC/ESC sequences at end-of-string are consumed
/// so `terminal_filter` never leaks partial escapes.
fn scan_escape(text: &str, start: usize, consume_unterminated: bool) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(start).copied() != Some(0x1b) {
        return None;
    }
    let next = match bytes.get(start + 1).copied() {
        Some(b) => b,
        None => return if consume_unterminated { Some(start + 1) } else { None },
    };

    if next == b'[' {
        // CSI: \x1b[ [0x30..=0x3f]* [0x20..=0x2f]* [0x40..=0x7e]
        let mut i = start + 2;
        while i < bytes.len() && (0x30..=0x3f).contains(&bytes[i]) {
            i += 1;
        }
        while i < bytes.len() && (0x20..=0x2f).contains(&bytes[i]) {
            i += 1;
        }
        if i < bytes.len() && (0x40..=0x7e).contains(&bytes[i]) {
            return Some(i + 1);
        }
        return if consume_unterminated { Some(i) } else { None };
    }

    if next == b']' {
        // OSC: \x1b] ... (\x07 | \x1b\\)
        let mut i = start + 2;
        while i < bytes.len() {
            if bytes[i] == 0x07 {
                return Some(i + 1);
            }
            if bytes[i] == 0x1b {
                if bytes.get(i + 1) == Some(&b'\\') {
                    return Some(i + 2);
                }
                // Nested ESC terminates malformed OSC
                return if consume_unterminated { Some(i) } else { None };
            }
            i += 1;
        }
        return if consume_unterminated { Some(bytes.len()) } else { None };
    }

    // 2-byte / intermediate ESC: \x1b [0x20..=0x2f]* [0x30..=0x7e]
    let mut i = start + 1;
    while i < bytes.len() && (0x20..=0x2f).contains(&bytes[i]) {
        i += 1;
    }
    if i < bytes.len() && (0x30..=0x7e).contains(&bytes[i]) {
        return Some(i + 1);
    }
    if consume_unterminated {
        Some(i)
    } else {
        None
    }
}

/// Strips ANSI escapes, replaces control/bidi characters with spaces, and collapses whitespace.
pub fn sanitize_text(input: &str) -> String {
    let stripped = strip_ansi(input);
    let mut out = String::with_capacity(stripped.len());
    let mut prev_space = true;
    for ch in stripped.chars() {
        if is_unsafe_control_or_bidi(ch) || ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Removes all CSI, OSC, and 2-byte ANSI escape sequences.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text.as_bytes()[i] == 0x1b
            && let Some(end) = scan_escape(text, i, false)
        {
            i = end;
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Computes terminal display column width of a single Unicode code point (East Asian Wide / Emoji = 2).
pub fn code_point_width(ch: char) -> usize {
    let cp = ch as u32;
    if cp <= 0x1f || (0x7f..=0x9f).contains(&cp) {
        return 0;
    }
    if (0x0300..=0x036f).contains(&cp)
        || (0x1ab0..=0x1aff).contains(&cp)
        || (0x200b..=0x200f).contains(&cp)
        || (0xfe00..=0xfe0f).contains(&cp)
        || (0xe0100..=0xe01ef).contains(&cp)
    {
        return 0;
    }
    if cp >= 0x1100
        && (cp <= 0x115f
            || cp == 0x2329
            || cp == 0x232a
            || (0x2e80..=0x303e).contains(&cp)
            || ((0x3040..=0xa4cf).contains(&cp) && cp != 0x303f)
            || (0xac00..=0xd7a3).contains(&cp)
            || (0xf900..=0xfaff).contains(&cp)
            || (0xfe10..=0xfe19).contains(&cp)
            || (0xfe30..=0xfe6f).contains(&cp)
            || (0xff00..=0xff60).contains(&cp)
            || (0xffe0..=0xffe6).contains(&cp)
            || (0x1f300..=0x1faff).contains(&cp)
            || (0x20000..=0x3fffd).contains(&cp))
    {
        return 2;
    }
    1
}

pub fn text_width(text: &str) -> usize {
    let plain = strip_ansi(text);
    plain.chars().map(code_point_width).sum()
}

/// Truncates an ANSI-styled string to fit within `max_cols` columns, preserving SGR resets
/// and cleanly closing any open OSC 8 hyperlink.
pub fn truncate_ansi(text: &str, max_cols: usize) -> String {
    truncate_ansi_with_ellipsis(text, max_cols, "…")
}

pub fn truncate_ansi_with_ellipsis(text: &str, max_cols: usize, ellipsis: &str) -> String {
    if max_cols == 0 {
        return String::new();
    }
    if text_width(text) <= max_cols {
        return text.to_string();
    }

    let ellipsis_width = text_width(ellipsis);
    let budget = max_cols.saturating_sub(ellipsis_width);

    let mut out = String::with_capacity(text.len());
    let mut used = 0usize;
    let mut open_osc8 = false;
    let mut i = 0usize;

    while i < text.len() {
        if text.as_bytes()[i] == 0x1b
            && let Some(end) = scan_escape(text, i, false)
        {
            let seq = &text[i..end];
            if seq.starts_with("\x1b]8;;") {
                open_osc8 = seq != "\x1b]8;;\x1b\\" && seq != "\x1b]8;;\x07";
            }
            out.push_str(seq);
            i = end;
            continue;
        }

        let ch = text[i..].chars().next().unwrap_or('\0');
        let w = code_point_width(ch);
        if used + w > budget {
            if open_osc8 {
                out.push_str("\x1b]8;;\x1b\\");
            }
            out.push_str(RESET);
            if max_cols >= ellipsis_width {
                out.push_str(ellipsis);
            }
            return out;
        }
        out.push(ch);
        used += w;
        i += ch.len_utf8();
    }

    if open_osc8 {
        out.push_str("\x1b]8;;\x1b\\");
    }
    out.push_str(RESET);
    if max_cols >= ellipsis_width {
        out.push_str(ellipsis);
    }
    out
}

pub fn paint(text: &str, codes: &[&str]) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(text.len() + codes.len() * 6 + RESET.len());
    for c in codes {
        out.push_str(c);
    }
    out.push_str(text);
    out.push_str(RESET);
    out
}

/// Builds a fine-grained Unicode progress bar (`█▓▒░·`).
pub fn render_bar(
    percent: f64,
    length: usize,
    warning_at: f64,
    critical_at: f64,
    normal_color: &str,
) -> String {
    let clamped = if percent.is_finite() {
        percent.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let exact = (clamped * (length as f64)) / 100.0;
    let full = exact.floor() as usize;
    let rem = (exact - (full as f64)) * 100.0;

    let bar_color = if clamped >= critical_at {
        colors::BRIGHT_RED
    } else if clamped >= warning_at {
        colors::BRIGHT_YELLOW
    } else {
        normal_color
    };

    let mut filled_part = String::new();
    let mut empty_part = String::new();
    for i in 0..length {
        if i < full {
            filled_part.push('█');
        } else if i == full && clamped > 0.0 {
            if rem >= 75.0 {
                filled_part.push('▓');
            } else if rem >= 50.0 {
                filled_part.push('▒');
            } else if rem >= 25.0 {
                filled_part.push('░');
            } else {
                empty_part.push('·');
            }
        } else {
            empty_part.push('·');
        }
    }

    format!("{bar_color}{filled_part}{}{empty_part}{RESET}", colors::GRAY)
}

fn path_to_file_url(abs_path: &str) -> Option<String> {
    if abs_path.is_empty() {
        return None;
    }
    let mut normalized = abs_path.replace('\\', "/");
    // Windows drive letter e.g. C:/Users/dev -> /C:/Users/dev
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        normalized.insert(0, '/');
    }
    if !normalized.starts_with('/') {
        return None;
    }

    let mut url = String::with_capacity("file://".len() + normalized.len() + 8);
    url.push_str("file://");
    for &b in normalized.as_bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~' | b':');
        if keep {
            url.push(b as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(url, "%{b:02X}");
        }
    }
    Some(url)
}

/// Formats an OSC 8 terminal hyperlink to a local file path (`file://`), permitted by `terminal_filter`.
pub fn file_link(abs_path: &str, label: &str, enabled: bool) -> String {
    if !enabled || abs_path.is_empty() {
        return label.to_string();
    }
    match path_to_file_url(abs_path) {
        Some(href) => format!("\x1b]8;;{href}\x1b\\{label}\x1b]8;;\x1b\\"),
        None => label.to_string(),
    }
}

fn is_allowed_sgr(seq: &str) -> bool {
    if !seq.starts_with("\x1b[") || !seq.ends_with('m') {
        return false;
    }
    let inner = &seq[2..seq.len() - 1];
    inner.bytes().all(|b| b.is_ascii_digit() || b == b';')
}

fn is_allowed_osc8(seq: &str) -> bool {
    let Some(rest) = seq.strip_prefix("\x1b]8;;") else {
        return false;
    };
    let Some(target) = rest.strip_suffix("\x1b\\") else {
        return false;
    };
    if target.is_empty() {
        return true;
    }
    if !target.starts_with("file://") {
        return false;
    }
    !target
        .chars()
        .any(|c| is_unsafe_control_or_bidi(c) || c.is_whitespace())
}

/// Terminal output allowlist firewall: preserves SGR color sequences (`\x1b[[0-9;]*m`) and
/// `file://` OSC 8 hyperlinks, while stripping OSC 52 clipboard writes, title hijacks,
/// cursor movement CSI sequences, C0/C1 control characters, and Unicode bidi overrides.
pub fn terminal_filter(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if line.as_bytes()[i] == 0x1b
            && let Some(end) = scan_escape(line, i, true)
        {
            let seq = &line[i..end];
            if is_allowed_sgr(seq) || is_allowed_osc8(seq) {
                out.push_str(seq);
            }
            i = end;
            continue;
        }
        let ch = line[i..].chars().next().unwrap_or('\0');
        if !is_unsafe_control_or_bidi(ch) {
            out.push(ch);
        }
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_width_handles_ascii_cjk_emoji_and_ansi() {
        assert_eq!(text_width("hello"), 5);
        assert_eq!(text_width("简体中文"), 8);
        assert_eq!(text_width("🔧 TOOL"), 7);
        assert_eq!(text_width(&paint("上下文 100%", &[colors::BRIGHT_GREEN])), 11);
        let linked = file_link("/Users/dev/agy-hud", &paint("agy-hud", &[colors::BRIGHT_WHITE]), true);
        assert_eq!(text_width(&linked), 7);
    }

    #[test]
    fn truncate_ansi_respects_cjk_boundaries_and_closes_osc8_links() {
        let cjk = paint("简体中文状态栏测试", &[colors::BRIGHT_CYAN]);
        for width in [5, 6, 8, 11] {
            let truncated = truncate_ansi(&cjk, width);
            assert!(
                text_width(&truncated) <= width,
                "Expected width <= {width}, got {}",
                text_width(&truncated)
            );
        }

        let linked = file_link(
            "/Users/dev/very-long-workspace-folder-name",
            "very-long-workspace-folder-name",
            true,
        );
        let clipped = truncate_ansi(&linked, 12);
        assert!(text_width(&clipped) <= 12);
        assert!(clipped.contains("\x1b]8;;\x1b\\"));
    }

    #[test]
    fn render_bar_produces_exact_visual_width() {
        for pct in [0.0, 6.2, 28.4, 65.0, 92.5, 100.0] {
            let bar = render_bar(pct, 12, 60.0, 85.0, colors::BRIGHT_GREEN);
            assert_eq!(text_width(&bar), 12);
        }
    }

    #[test]
    fn terminal_filter_strips_hostile_escapes_while_preserving_sgr_and_file_links() {
        let safe = format!(
            "{}READY\x1b[0m {}",
            colors::BRIGHT_GREEN,
            file_link("/tmp/demo", "demo", true)
        );
        assert_eq!(terminal_filter(&safe), safe);

        let hostile = concat!(
            "ok\x1b]52;c;cHduZWQ=\x07\x1b]2;spoofed-title\x07\x1b[2J\x1b[H",
            "\x1b]8;;https://evil.example/phish\x1b\\click\x1b]8;;\x1b\\\u{202e}hidden"
        );
        let cleaned = terminal_filter(hostile);
        assert_eq!(strip_ansi(&cleaned), "okclickhidden");
        assert!(!cleaned.contains("52;c;"));
        assert!(!cleaned.contains("spoofed-title"));
        assert!(!cleaned.contains("evil.example"));
        assert!(!cleaned.contains('\u{202e}'));
    }

    #[test]
    fn sanitize_text_removes_newlines_ansi_and_bidi() {
        assert_eq!(
            sanitize_text("  Gemini\n4\t\x1b[31mArgon\u{202e}  "),
            "Gemini 4 Argon"
        );
    }
}
