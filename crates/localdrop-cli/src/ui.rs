//! UI utilities for LocalDrop CLI.

use std::time::Duration;

const BOX_WIDTH: usize = 33;

/// A formatted box for displaying share codes.
pub struct CodeBox<'a> {
    code: &'a str,
    expire: Option<&'a str>,
}

impl<'a> CodeBox<'a> {
    /// Create a new code box.
    #[must_use]
    pub const fn new(code: &'a str) -> Self {
        Self { code, expire: None }
    }

    /// Add expiration time to the box.
    #[must_use]
    pub const fn with_expire(mut self, expire: &'a str) -> Self {
        self.expire = Some(expire);
        self
    }

    /// Display the code box to stdout.
    pub fn display(&self) {
        let spaced_code = format_code_spaced(self.code);
        let code_line = format!("Code:  {}", spaced_code);

        println!("  ┌{}┐", "─".repeat(BOX_WIDTH));
        println!("  │{}│", " ".repeat(BOX_WIDTH));
        println!("  │{}│", center_in_box(&code_line, BOX_WIDTH));
        println!("  │{}│", " ".repeat(BOX_WIDTH));

        if let Some(expire) = self.expire {
            let expire_line = format!("Expires in {}", expire);
            println!("  │{}│", center_in_box(&expire_line, BOX_WIDTH));
            println!("  │{}│", " ".repeat(BOX_WIDTH));
        }

        println!("  └{}┘", "─".repeat(BOX_WIDTH));
    }
}

fn format_code_spaced(code: &str) -> String {
    code.chars()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn center_in_box(content: &str, width: usize) -> String {
    let content_len = content.chars().count();
    let padding = width.saturating_sub(content_len);
    let left = padding / 2;
    let right = padding - left;
    format!("{}{}{}", " ".repeat(left), content, " ".repeat(right))
}

/// Parse a duration string like "5m", "30s", or "1h".
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    if let Some(num_str) = s.strip_suffix('m') {
        let num: u64 = num_str.parse().ok()?;
        Some(Duration::from_secs(num * 60))
    } else if let Some(num_str) = s.strip_suffix('s') {
        let num: u64 = num_str.parse().ok()?;
        Some(Duration::from_secs(num))
    } else if let Some(num_str) = s.strip_suffix('h') {
        let num: u64 = num_str.parse().ok()?;
        Some(Duration::from_secs(num * 3600))
    } else {
        None
    }
}

/// Format remaining duration as "M:SS".
pub fn format_remaining(remaining: Duration) -> String {
    let total_secs = remaining.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{}:{:02}", mins, secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_code_spaced() {
        assert_eq!(format_code_spaced("A7K9"), "A 7 K 9");
        assert_eq!(format_code_spaced("ABCD"), "A B C D");
    }

    #[test]
    fn test_center_in_box() {
        let centered = center_in_box("hello", 11);
        assert_eq!(centered, "   hello   ");

        let centered = center_in_box("hi", 6);
        assert_eq!(centered, "  hi  ");
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("10m"), Some(Duration::from_secs(600)));
        assert_eq!(parse_duration("invalid"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn test_format_remaining() {
        assert_eq!(format_remaining(Duration::from_secs(300)), "5:00");
        assert_eq!(format_remaining(Duration::from_secs(65)), "1:05");
        assert_eq!(format_remaining(Duration::from_secs(0)), "0:00");
    }
}
