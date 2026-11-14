use crate::types::GlyphMode;

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
        None => {
            return if consume_unterminated {
                Some(start + 1)
            } else {
                None
            };
        }
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
        return if consume_unterminated {
            Some(bytes.len())
        } else {
            None
        };
    }

    // 2-byte / intermediate ESC: \x1b [0x20..=0x2f]* [0x30..=0x7e]
    let mut i = start + 1;
    while i < bytes.len() && (0x20..=0x2f).contains(&bytes[i]) {
        i += 1;
    }
    if i < bytes.len() && (0x30..=0x7e).contains(&bytes[i]) {
        return Some(i + 1);
    }
    if consume_unterminated { Some(i) } else { None }
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

/// Sanitizes free-form `conversation_title` text to a single plain line: strips ANSI escapes,
/// control/bidi overrides, and invisible format characters (`U+200B`, `U+FEFF`, `U+2060`), while
/// preserving ZWNJ (`U+200C`), ZWJ (`U+200D`), variation selectors, and emoji tag characters.
pub fn sanitize_title_text(input: &str) -> String {
    let stripped = strip_ansi(input);
    let mut out = String::with_capacity(stripped.len());
    let mut prev_space = true;
    for ch in stripped.chars() {
        let cp = ch as u32;
        if is_unsafe_control_or_bidi(ch) || ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        // Drop invisible format chars except ZWNJ, ZWJ, variation selectors, and emoji tags
        let drop_invisible = matches!(cp, 0x00ad | 0x200b | 0x2060..=0x2064 | 0xfeff | 0xe0001);
        if drop_invisible {
            continue;
        }
        out.push(ch);
        prev_space = false;
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

fn is_regional_indicator(cp: u32) -> bool {
    (0x1f1e6..=0x1f1ff).contains(&cp)
}

fn is_grapheme_extend(cp: u32) -> bool {
    (0x0300..=0x036f).contains(&cp)
        || (0x0483..=0x0489).contains(&cp)
        || (0x0591..=0x05bd).contains(&cp)
        || matches!(cp, 0x05bf | 0x05c1..=0x05c2 | 0x05c4..=0x05c5 | 0x05c7)
        || (0x0610..=0x061a).contains(&cp)
        || (0x064b..=0x065f).contains(&cp)
        || cp == 0x0670
        || (0x06d6..=0x06ed).contains(&cp)
        || cp == 0x0e31
        || (0x0e34..=0x0e3a).contains(&cp)
        || (0x0e47..=0x0e4e).contains(&cp)
        || (0x1ab0..=0x1aff).contains(&cp)
        || (0x1dc0..=0x1dff).contains(&cp)
        || cp == 0x200c
        || (0x20d0..=0x20ff).contains(&cp)
        || (0xfe00..=0xfe0f).contains(&cp)
        || (0xfe20..=0xfe2f).contains(&cp)
        || (0x1f3fb..=0x1f3ff).contains(&cp)
        || (0xe0020..=0xe007f).contains(&cp)
        || (0xe0100..=0xe01ef).contains(&cp)
}

/// Zero-dependency extended Unicode grapheme cluster iterator (UAX #29 core terminal rules:
/// CRLF, combining/enclosing marks, variation selectors, skin-tone modifiers, emoji tag sequences,
/// ZWJ `\u{200d}` multi-emoji sequences, and Regional Indicator flag pairs).
pub struct Graphemes<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Graphemes<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let mut iter = self.rest.char_indices();
        let (_, first) = iter.next()?;
        let first_cp = first as u32;

        if first == '\r' {
            if let Some((_, '\n')) = iter.next() {
                let (cluster, tail) = self.rest.split_at(2);
                self.rest = tail;
                return Some(cluster);
            }
            let (cluster, tail) = self.rest.split_at(1);
            self.rest = tail;
            return Some(cluster);
        }
        if first == '\n' || (first_cp <= 0x1f || (0x7f..=0x9f).contains(&first_cp)) {
            let len = first.len_utf8();
            let (cluster, tail) = self.rest.split_at(len);
            self.rest = tail;
            return Some(cluster);
        }

        let mut end = first.len_utf8();
        let mut ri_paired = false;
        let mut prev_was_zwj = first == '\u{200d}';

        for (idx, ch) in iter {
            let cp = ch as u32;
            if ch == '\r' || ch == '\n' || cp <= 0x1f || (0x7f..=0x9f).contains(&cp) {
                break;
            }
            if prev_was_zwj {
                end = idx + ch.len_utf8();
                prev_was_zwj = ch == '\u{200d}';
                continue;
            }
            if ch == '\u{200d}' {
                end = idx + ch.len_utf8();
                prev_was_zwj = true;
                continue;
            }
            if is_grapheme_extend(cp) {
                end = idx + ch.len_utf8();
                continue;
            }
            if is_regional_indicator(first_cp) && !ri_paired && is_regional_indicator(cp) {
                end = idx + ch.len_utf8();
                ri_paired = true;
                continue;
            }
            break;
        }

        let (cluster, tail) = self.rest.split_at(end);
        self.rest = tail;
        Some(cluster)
    }
}

pub fn graphemes(text: &str) -> Graphemes<'_> {
    Graphemes { rest: text }
}

/// Computes terminal display column width of a single Unicode code point (East Asian Wide / Emoji = 2).
pub fn code_point_width(ch: char) -> usize {
    let cp = ch as u32;
    if cp <= 0x1f || (0x7f..=0x9f).contains(&cp) {
        return 0;
    }
    if is_grapheme_extend(cp) || (0x200b..=0x200f).contains(&cp) {
        return 0;
    }
    if cp >= 0x1100
        && (cp <= 0x115f
            || matches!(
                cp,
                0x231a..=0x231b
                    | 0x2329..=0x232a
                    | 0x23e9..=0x23f3
                    | 0x25fd..=0x25fe
                    | 0x2614..=0x2615
                    | 0x2648..=0x2653
                    | 0x267f
                    | 0x2693
                    | 0x26a1
                    | 0x26aa..=0x26ab
                    | 0x26bd..=0x26be
                    | 0x26c4..=0x26c5
                    | 0x26ce
                    | 0x26d4
                    | 0x26ea
                    | 0x26f2..=0x26f3
                    | 0x26f5
                    | 0x26fa
                    | 0x26fd
                    | 0x2705
                    | 0x270a..=0x270b
                    | 0x2728
                    | 0x274c
                    | 0x274e
                    | 0x2753..=0x2755
                    | 0x2757
                    | 0x2795..=0x2797
                    | 0x27b0
                    | 0x27bf
                    | 0x2b1b..=0x2b1c
                    | 0x2b50
                    | 0x2b55
            )
            || (0x2e80..=0x303e).contains(&cp)
            || ((0x3040..=0xa4cf).contains(&cp) && cp != 0x303f)
            || (0xac00..=0xd7a3).contains(&cp)
            || (0xf900..=0xfaff).contains(&cp)
            || (0xfe10..=0xfe19).contains(&cp)
            || (0xfe30..=0xfe6f).contains(&cp)
            || (0xff00..=0xff60).contains(&cp)
            || (0xffe0..=0xffe6).contains(&cp)
            || is_regional_indicator(cp)
            || (0x1f300..=0x1faff).contains(&cp)
            || (0x20000..=0x3fffd).contains(&cp))
    {
        return 2;
    }
    1
}

/// Computes terminal display column width of a complete extended grapheme cluster.
pub fn grapheme_width(cluster: &str) -> usize {
    if cluster.is_empty() {
        return 0;
    }
    let mut has_vs16_or_keycap = false;
    let mut max_w = 0usize;
    for ch in cluster.chars() {
        if matches!(ch, '\u{fe0f}' | '\u{20e3}') {
            has_vs16_or_keycap = true;
        }
        let w = code_point_width(ch);
        if w > max_w {
            max_w = w;
        }
    }
    if max_w == 0 {
        0
    } else if has_vs16_or_keycap || max_w >= 2 {
        2
    } else {
        1
    }
}

pub fn text_width(text: &str) -> usize {
    let plain = strip_ansi(text);
    graphemes(&plain).map(grapheme_width).sum()
}

/// Masks sensitive plain text while preserving exact terminal column widths and path separators
/// (`'/'` and `'\\'` preserved, 2-column CJK/Emoji clusters -> `'全'`, 1-column clusters -> `'x'`).
pub fn mask_preserving_width(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for cluster in graphemes(text) {
        if cluster == "/" || cluster == "\\" {
            out.push_str(cluster);
            continue;
        }
        match grapheme_width(cluster) {
            0 => {}
            1 => out.push('x'),
            _ => out.push('全'),
        }
    }
    out
}

/// Truncates an ANSI-styled string to fit within `max_cols` columns, preserving SGR resets,
/// respecting extended Unicode grapheme cluster boundaries, and cleanly closing any open OSC 8 hyperlink.
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

        // Advance up to the next ESC byte so grapheme clusters never cross ANSI sequences
        let next_esc = text.as_bytes()[i..]
            .iter()
            .position(|&b| b == 0x1b)
            .map_or(text.len(), |rel| i + rel);

        for cluster in graphemes(&text[i..next_esc]) {
            let w = grapheme_width(cluster);
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
            out.push_str(cluster);
            used += w;
        }
        i = next_esc;
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
    if codes.is_empty() {
        return text.to_string();
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
    render_bar_with_mode(
        percent,
        length,
        warning_at,
        critical_at,
        normal_color,
        GlyphMode::Unicode,
    )
}

/// Builds a progress bar using either fine-grained Unicode blocks (`█▓▒░·`) or 7-bit ASCII (`#=-`).
pub fn render_bar_with_mode(
    percent: f64,
    length: usize,
    warning_at: f64,
    critical_at: f64,
    normal_color: &str,
    glyph_mode: GlyphMode,
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
        colors::RED
    } else if clamped >= warning_at {
        colors::YELLOW
    } else {
        normal_color
    };

    let (full_ch, c75, c50, c25, empty_ch) = match glyph_mode {
        GlyphMode::Ascii => ('#', '#', '=', '=', '-'),
        GlyphMode::Unicode | GlyphMode::Nerd => ('█', '▓', '▒', '░', '·'),
    };

    let mut filled_part = String::new();
    let mut empty_part = String::new();
    for i in 0..length {
        if i < full {
            filled_part.push(full_ch);
        } else if i == full && clamped > 0.0 {
            if rem >= 75.0 {
                filled_part.push(c75);
            } else if rem >= 50.0 {
                filled_part.push(c50);
            } else if rem >= 25.0 {
                filled_part.push(c25);
            } else {
                empty_part.push(empty_ch);
            }
        } else {
            empty_part.push(empty_ch);
        }
    }

    format!("{bar_color}{filled_part}{RESET}{DIM}{empty_part}{RESET}")
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
        let keep =
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~' | b':');
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

/// Validates a canonical `https://github.com/<owner>/<repo>` URL prefix.
pub fn is_valid_github_repo_prefix(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://github.com/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(repo) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let valid_seg = |seg: &str| {
        !seg.is_empty()
            && seg.len() <= 100
            && seg != "."
            && seg != ".."
            && seg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    valid_seg(owner) && valid_seg(repo)
}

/// Formats an OSC 8 hyperlink to a GitHub branch (`https://github.com/<owner>/<repo>/tree/<branch>`),
/// permitted strictly by `terminal_filter_with_repo` when matching the workspace's GitHub remote.
pub fn github_branch_link(repo_url: &str, branch: &str, label: &str, enabled: bool) -> String {
    if !enabled
        || branch.is_empty()
        || branch == "detached"
        || branch.starts_with('@')
        || !is_valid_github_repo_prefix(repo_url)
    {
        return label.to_string();
    }
    let mut href = String::with_capacity(repo_url.len() + "/tree/".len() + branch.len() + 8);
    href.push_str(repo_url);
    href.push_str("/tree/");
    for &b in branch.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~');
        if keep {
            href.push(b as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(href, "%{b:02X}");
        }
    }
    format!("\x1b]8;;{href}\x1b\\{label}\x1b]8;;\x1b\\")
}

fn is_allowed_sgr(seq: &str) -> bool {
    if !seq.starts_with("\x1b[") || !seq.ends_with('m') {
        return false;
    }
    let inner = &seq[2..seq.len() - 1];
    inner.bytes().all(|b| b.is_ascii_digit() || b == b';')
}

fn is_allowed_osc8(seq: &str, allowed_github_repo: Option<&str>) -> bool {
    let Some(rest) = seq.strip_prefix("\x1b]8;;") else {
        return false;
    };
    let Some(target) = rest.strip_suffix("\x1b\\") else {
        return false;
    };
    if target.is_empty() {
        return true;
    }
    if target
        .chars()
        .any(|c| is_unsafe_control_or_bidi(c) || c.is_whitespace())
    {
        return false;
    }
    if target.starts_with("file://") {
        return true;
    }
    if let Some(repo) = allowed_github_repo
        && is_valid_github_repo_prefix(repo)
        && let Some(after_repo) = target.strip_prefix(repo)
        && let Some(branch_part) = after_repo.strip_prefix("/tree/")
        && !branch_part.is_empty()
    {
        return branch_part.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~' | b'%')
        });
    }
    false
}

/// Terminal output allowlist firewall: preserves SGR color sequences (`\x1b[[0-9;]*m`) and
/// `file://` OSC 8 hyperlinks, while stripping OSC 52 clipboard writes, title hijacks,
/// cursor movement CSI sequences, C0/C1 control characters, and Unicode bidi overrides.
pub fn terminal_filter(line: &str) -> String {
    terminal_filter_with_repo(line, None)
}

/// Capability-scoped terminal output firewall: additionally permits `https://github.com/<owner>/<repo>/tree/...`
/// OSC 8 branch hyperlinks strictly matching `allowed_github_repo`.
pub fn terminal_filter_with_repo(line: &str, allowed_github_repo: Option<&str>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if line.as_bytes()[i] == 0x1b
            && let Some(end) = scan_escape(line, i, true)
        {
            let seq = &line[i..end];
            if is_allowed_sgr(seq) || is_allowed_osc8(seq, allowed_github_repo) {
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
        assert_eq!(text_width("⏱ 12m"), 6);
        assert_eq!(text_width("⚡ 98%"), 6);
        assert_eq!(text_width(&paint("上下文 100%", &[colors::GREEN])), 11);
        let linked = file_link("/Users/dev/agy-hud", &paint("agy-hud", &[BOLD]), true);
        assert_eq!(text_width(&linked), 7);
    }

    #[test]
    fn grapheme_clusters_preserve_zwj_flags_skin_tones_and_mask_widths() {
        // ZWJ coder, rainbow flag, skin-toned thumbs-up, and CN flag are each 1 cluster of width 2
        assert_eq!(graphemes("👨‍💻🏳️‍🌈👍🏽🇨🇳").count(), 4);
        assert_eq!(text_width("👨‍💻"), 2);
        assert_eq!(text_width("🏳️‍🌈"), 2);
        assert_eq!(text_width("👍🏽"), 2);
        assert_eq!(text_width("🇨🇳🇺🇸"), 4);

        // Truncating a string of multi-codepoint emojis must never leave half a flag or trailing ZWJ
        let emoji_run = "👨‍💻🏳️‍🌈👍🏽🇨🇳";
        for max_w in [1, 2, 3, 4, 5, 6, 7] {
            let clipped = truncate_ansi(emoji_run, max_w);
            let plain = strip_ansi(&clipped);
            assert!(text_width(&clipped) <= max_w);
            assert!(!plain.contains('\u{fffd}'));
            assert!(!plain.ends_with('\u{200d}'));
            // Ensure no lone Regional Indicator was split from `🇨🇳`
            let ri_count = plain
                .chars()
                .filter(|&c| is_regional_indicator(c as u32))
                .count();
            assert_eq!(
                ri_count % 2,
                0,
                "Split Regional Indicator flag at width {max_w}: {plain:?}"
            );
        }

        // Width-preserving mask preserves '/' and '\\' and exact terminal display width
        let sample_path = "/Users/明/secret-项目👨‍💻\\sub";
        let masked = mask_preserving_width(sample_path);
        assert_eq!(masked, "/xxxxx/全/xxxxxxx全全全\\xxx");
        assert_eq!(text_width(&masked), text_width(sample_path));
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

        let repo = "https://github.com/maxlee/agy-hud";
        let branch_linked = github_branch_link(repo, "feat/hud", "feat/hud", true);
        assert_eq!(
            terminal_filter_with_repo(&branch_linked, Some(repo)),
            branch_linked
        );
        // Without matching repo capability, or pointing to a foreign repo, https OSC 8 is stripped!
        assert_eq!(strip_ansi(&terminal_filter(&branch_linked)), "feat/hud");
        assert!(!terminal_filter(&branch_linked).contains("github.com"));
        let foreign = github_branch_link("https://github.com/attacker/evil", "main", "main", true);
        assert!(!terminal_filter_with_repo(&foreign, Some(repo)).contains("attacker"));

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

    #[test]
    fn speed_tier_emojis_have_exact_two_column_width_and_safe_truncation() {
        for emoji in ["🐢", "🛵", "🚗", "🚅", "✈️", "🚀", "🛰️", "🛸", "👽"] {
            assert_eq!(
                graphemes(emoji).count(),
                1,
                "Expected 1 grapheme cluster for {emoji:?}"
            );
            assert_eq!(
                grapheme_width(emoji),
                2,
                "Expected grapheme_width == 2 for {emoji:?}"
            );
            assert_eq!(
                text_width(emoji),
                2,
                "Expected text_width == 2 for {emoji:?}"
            );
            assert_eq!(
                text_width(&format!("{emoji} ~68 tok/s (42~112)")),
                21,
                "Expected full badge width == 21 for {emoji:?}"
            );
        }

        // Truncating around variation-selector emojis must never split base char from U+FE0F
        for sample in ["✈️ ~260 tok/s", "🛰️ ~350 tok/s", "🏍️ ~120 tok/s"] {
            for max_w in 1..=6 {
                let clipped = truncate_ansi(sample, max_w);
                let plain = strip_ansi(&clipped);
                assert!(text_width(&clipped) <= max_w);
                assert!(!plain.starts_with('\u{fe0f}'));
                if plain.contains('\u{fe0f}') {
                    assert!(
                        plain.contains('\u{2708}')
                            || plain.contains('\u{1f6f0}')
                            || plain.contains('\u{1f3cd}')
                    );
                }
            }
        }
    }
}
