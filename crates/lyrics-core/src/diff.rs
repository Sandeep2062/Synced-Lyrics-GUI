use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLineType {
    Header,
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub line_type: DiffLineType,
    pub text: String,
}

impl DiffLine {
    pub fn to_unified_string(&self) -> String {
        match self.line_type {
            DiffLineType::Header => self.text.clone(),
            DiffLineType::Context => format!("  {}", self.text),
            DiffLineType::Added => format!("+ {}", self.text),
            DiffLineType::Removed => format!("- {}", self.text),
        }
    }
}

/// Computes a line-by-line diff using Longest Common Subsequence (LCS).
pub fn compute_line_diff(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    if old_lines.is_empty() && new_lines.is_empty() {
        return Vec::new();
    }

    if old_lines.is_empty() {
        return new_lines
            .into_iter()
            .map(|line| DiffLine {
                line_type: DiffLineType::Added,
                text: line.to_string(),
            })
            .collect();
    }

    if new_lines.is_empty() {
        return old_lines
            .into_iter()
            .map(|line| DiffLine {
                line_type: DiffLineType::Removed,
                text: line.to_string(),
            })
            .collect();
    }

    let n = old_lines.len();
    let m = new_lines.len();

    // 2D dynamic programming table for LCS
    let mut dp = vec![vec![0u16; m + 1]; n + 1];

    for i in 0..n {
        for j in 0..m {
            if old_lines[i] == new_lines[j] {
                dp[i + 1][j + 1] = dp[i][j].saturating_add(1);
            } else {
                dp[i + 1][j + 1] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    // Backtrack to reconstruct the diff
    let mut result = Vec::new();
    let mut i = n;
    let mut j = m;

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old_lines[i - 1] == new_lines[j - 1] {
            result.push(DiffLine {
                line_type: DiffLineType::Context,
                text: old_lines[i - 1].to_string(),
            });
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            result.push(DiffLine {
                line_type: DiffLineType::Added,
                text: new_lines[j - 1].to_string(),
            });
            j -= 1;
        } else if i > 0 && (j == 0 || dp[i][j - 1] < dp[i - 1][j]) {
            result.push(DiffLine {
                line_type: DiffLineType::Removed,
                text: old_lines[i - 1].to_string(),
            });
            i -= 1;
        }
    }

    result.reverse();
    result
}

/// Generates a standard unified diff string between old_text and new_text.
pub fn generate_unified_diff(
    old_text: &str,
    new_text: &str,
    old_label: &str,
    new_label: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- {}\n", old_label));
    out.push_str(&format!("+++ {}\n", new_label));

    let lines = compute_line_diff(old_text, new_text);
    for line in lines {
        out.push_str(&line.to_unified_string());
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_empty_to_new() {
        let old = "";
        let new = "[00:01.00]First line\n[00:04.00]Second line";
        let diff = compute_line_diff(old, new);
        assert_eq!(diff.len(), 2);
        assert_eq!(diff[0].line_type, DiffLineType::Added);
        assert_eq!(diff[1].line_type, DiffLineType::Added);

        let unified = generate_unified_diff(old, new, "Missing", "LRCLIB");
        assert!(unified.contains("--- Missing"));
        assert!(unified.contains("+++ LRCLIB"));
        assert!(unified.contains("+ [00:01.00]First line"));
        assert!(unified.contains("+ [00:04.00]Second line"));
    }

    #[test]
    fn test_diff_offset_modification() {
        let old = "[00:01.00]Line 1\n[00:05.00]Line 2";
        let new = "[00:02.50]Line 1\n[00:06.50]Line 2";
        let diff = compute_line_diff(old, new);
        assert_eq!(diff.len(), 4);
        assert_eq!(diff[0].line_type, DiffLineType::Removed);
        assert_eq!(diff[1].line_type, DiffLineType::Removed);
        assert_eq!(diff[2].line_type, DiffLineType::Added);
        assert_eq!(diff[3].line_type, DiffLineType::Added);
    }

    #[test]
    fn test_diff_identical_lines() {
        let old = "[00:01.00]Line 1\n[00:05.00]Line 2";
        let new = "[00:01.00]Line 1\n[00:05.00]Line 2";
        let diff = compute_line_diff(old, new);
        assert_eq!(diff.len(), 2);
        assert_eq!(diff[0].line_type, DiffLineType::Context);
        assert_eq!(diff[1].line_type, DiffLineType::Context);
    }
}
