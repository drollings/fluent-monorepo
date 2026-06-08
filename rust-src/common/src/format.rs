use std::fmt;

pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub fn parse_size(size_str: &str) -> Option<u64> {
    let s = size_str.trim();
    if s.is_empty() {
        return None;
    }
    let (num_part, suffix) = s
        .chars()
        .partition::<String, _>(|c| c.is_ascii_digit() || *c == '.');
    let num: f64 = num_part.parse().ok()?;
    let multiplier = match suffix.trim().to_uppercase().as_str() {
        "B" | "" => 1,
        "KB" => 1024,
        "MB" => 1024 * 1024,
        "GB" => 1024 * 1024 * 1024,
        "TB" => 1024u64 * 1024 * 1024 * 1024,
        _ => return None,
    };
    Some((num * multiplier as f64) as u64)
}

#[derive(Debug, Clone)]
pub struct Column {
    pub header: String,
    pub key: String,
    pub width: usize,
    pub align_left: bool,
}

impl Column {
    pub fn effective_width(&self) -> usize {
        if self.width > 0 {
            self.width
        } else {
            self.header.len() + 2
        }
    }
}

pub struct Table {
    pub columns: Vec<Column>,
    pub rows: Vec<serde_json::Value>,
    pub title: String,
}

impl Table {
    pub fn new(columns: Vec<Column>, title: &str) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            title: title.to_string(),
        }
    }

    pub fn with_rows(&mut self, rows: Vec<serde_json::Value>) {
        self.rows = rows;
    }

    pub fn render(&self) -> String {
        if self.columns.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        if !self.title.is_empty() {
            out.push_str(&self.title);
            out.push('\n');
            out.push_str(&"-".repeat(self.title.len()));
            out.push('\n');
        }
        for col in &self.columns {
            let w = col.effective_width();
            if col.align_left {
                out.push_str(&format!(" {:<w$}", col.header, w = w));
            } else {
                out.push_str(&format!(" {:>w$}", col.header, w = w));
            }
        }
        out.push('\n');
        for row in &self.rows {
            for col in &self.columns {
                let val = row.get(&col.key).map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    _ => v.to_string(),
                }).unwrap_or_default();
                let w = col.effective_width();
                if col.align_left {
                    out.push_str(&format!(" {:<w$}", val, w = w));
                } else {
                    out.push_str(&format!(" {:>w$}", val, w = w));
                }
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(500), "500 B");
    }

    #[test]
    fn format_size_kb() {
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
    }

    #[test]
    fn parse_size_plain_number() {
        assert_eq!(parse_size("1024"), Some(1024));
    }

    #[test]
    fn parse_size_kb() {
        assert_eq!(parse_size("1 KB"), Some(1024));
    }

    #[test]
    fn parse_size_mb() {
        assert_eq!(parse_size("2 MB"), Some(2 * 1024 * 1024));
    }
}
