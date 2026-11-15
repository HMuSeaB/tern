//! 终端表格：按显示宽度对齐（中文、全角符号占两列），数字列右对齐。

pub struct Table<const N: usize> {
    header: [String; N],
    rows: Vec<[String; N]>,
}

impl<const N: usize> Table<N> {
    pub fn new(header: [&str; N]) -> Self {
        Self {
            header: header.map(str::to_string),
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: [String; N]) {
        self.rows.push(cells);
    }

    pub fn print(&self, indent: &str) {
        for line in self.render() {
            println!("{indent}{line}");
        }
    }

    fn render(&self) -> Vec<String> {
        let mut widths = self.header.each_ref().map(|h| display_width(h));
        for row in &self.rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(display_width(cell));
            }
        }
        // 除第一列外，所有单元格都像数字（或是 -）的列右对齐
        let numeric: Vec<bool> = (0..N)
            .map(|i| {
                i > 0 && !self.rows.is_empty() && self.rows.iter().all(|row| looks_numeric(&row[i]))
            })
            .collect();

        let line = |cells: &[String; N]| {
            let padded: Vec<String> = cells
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let pad = " ".repeat(widths[i] - display_width(cell));
                    if numeric[i] {
                        format!("{pad}{cell}")
                    } else {
                        format!("{cell}{pad}")
                    }
                })
                .collect();
            padded.join("  ").trim_end().to_string()
        };
        std::iter::once(line(&self.header))
            .chain(self.rows.iter().map(line))
            .collect()
    }
}

fn looks_numeric(cell: &str) -> bool {
    let cell = cell.trim();
    cell == "-"
        || cell.starts_with('$')
        || cell.starts_with("<$")
        || cell
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '.')
            && cell
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | '%' | 'K' | 'M' | 'B' | 's' | ','))
}

/// 东亚宽字符算两列。只覆盖常见区段，足够对齐中文表头和模型名
pub fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| {
            let cp = c as u32;
            let wide = matches!(cp,
                0x1100..=0x115F
                | 0x2E80..=0x303E
                | 0x3041..=0x33FF
                | 0x3400..=0x4DBF
                | 0x4E00..=0x9FFF
                | 0xA000..=0xA4CF
                | 0xAC00..=0xD7A3
                | 0xF900..=0xFAFF
                | 0xFE30..=0xFE4F
                | 0xFF00..=0xFF60
                | 0xFFE0..=0xFFE6
                | 0x1F300..=0x1F64F
                | 0x20000..=0x3FFFD);
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_cjk_and_right_aligns_numbers() {
        let mut table = Table::new(["名称", "花费", "说明"]);
        table.row(["主对话".into(), "$1.00".into(), "ok".into()]);
        table.row(["a".into(), "$12.50".into(), "中文".into()]);
        let lines = table.render();
        // 第一列宽 6（主对话），第二列宽 6（$12.50）右对齐
        assert_eq!(lines[0], "名称      花费  说明");
        assert_eq!(lines[1], "主对话   $1.00  ok");
        assert_eq!(lines[2], "a       $12.50  中文");
        let widths: Vec<usize> = lines.iter().map(|l| display_width(l)).collect();
        assert_eq!(widths, [20, 18, 20]);
    }

    #[test]
    fn width_counts_wide_chars_twice() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("未定价"), 6);
        assert_eq!(display_width("（未路由）"), 10);
    }
}
