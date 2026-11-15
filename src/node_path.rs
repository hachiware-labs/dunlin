//! `path.resolve` as Node.js computes it.
//!
//! Resolved paths become part of configuration identifiers, so they must be the same strings the
//! JavaScript implementation produced, including separators and drive letter case.

/// `path.resolve(base, path)` for the current platform.
pub fn resolve(base: &str, path: &str) -> String {
    if cfg!(windows) {
        win32_resolve(base, path)
    } else {
        posix_resolve(base, path)
    }
}

/// `path.dirname(path)` for an absolute path on the current platform.
pub fn dirname(path: &str) -> String {
    let separators: &[char] = if cfg!(windows) { &['\\', '/'] } else { &['/'] };
    let trimmed = path.trim_end_matches(separators);
    match trimmed.rfind(separators) {
        Some(0) => path[..1].to_owned(),
        Some(index) if cfg!(windows) && index == 2 && trimmed.as_bytes().get(1) == Some(&b':') => {
            trimmed[..3].to_owned()
        }
        Some(index) => trimmed[..index].to_owned(),
        None => ".".to_owned(),
    }
}

pub fn is_absolute(path: &str) -> bool {
    if cfg!(windows) {
        let bytes = path.as_bytes();
        matches!(bytes.first(), Some(b'\\' | b'/'))
            || (bytes.len() > 2
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/'))
    } else {
        path.starts_with('/')
    }
}

fn win32_separator(c: char) -> bool {
    c == '\\' || c == '/'
}

/// The device (`C:` or `\\server\share`), whether the path is absolute, and where the rest starts.
fn win32_root(path: &str) -> (String, bool, usize) {
    let chars: Vec<char> = path.chars().collect();
    let byte = |index: usize| chars[..index].iter().map(|c| c.len_utf8()).sum::<usize>();
    if chars.len() > 1 && win32_separator(chars[0]) && win32_separator(chars[1]) {
        // \\server\share
        let mut j = 2;
        while j < chars.len() && !win32_separator(chars[j]) {
            j += 1;
        }
        if j < chars.len() && j > 2 {
            let server: String = chars[2..j].iter().collect();
            while j < chars.len() && win32_separator(chars[j]) {
                j += 1;
            }
            let share_start = j;
            while j < chars.len() && !win32_separator(chars[j]) {
                j += 1;
            }
            if j > share_start {
                let share: String = chars[share_start..j].iter().collect();
                return (format!("\\\\{server}\\{share}"), true, byte(j));
            }
        }
        return (String::new(), true, byte(1));
    }
    if chars.first().is_some_and(|c| win32_separator(*c)) {
        return (String::new(), true, byte(1));
    }
    if chars.len() >= 2 && chars[0].is_ascii_alphabetic() && chars[1] == ':' {
        let device: String = chars[..2].iter().collect();
        if chars.len() > 2 && win32_separator(chars[2]) {
            return (device, true, 3);
        }
        return (device, false, 2);
    }
    (String::new(), false, 0)
}

pub fn win32_resolve(base: &str, path: &str) -> String {
    let mut device = String::new();
    let mut tail = String::new();
    let mut absolute = false;
    for part in [path, base] {
        if part.is_empty() {
            continue;
        }
        let (part_device, part_absolute, rest) = win32_root(part);
        if !part_device.is_empty()
            && !device.is_empty()
            && !part_device.eq_ignore_ascii_case(&device)
        {
            continue;
        }
        if device.is_empty() && !part_device.is_empty() {
            device = part_device;
        }
        if !absolute {
            tail = format!("{}\\{tail}", &part[rest..]);
            absolute = part_absolute;
        }
        if absolute && !device.is_empty() {
            break;
        }
    }
    let tail = normalize(&tail, !absolute, '\\', win32_separator);
    if absolute {
        format!("{device}\\{tail}")
    } else if device.is_empty() && tail.is_empty() {
        ".".to_owned()
    } else {
        format!("{device}{tail}")
    }
}

pub fn posix_resolve(base: &str, path: &str) -> String {
    let mut tail = String::new();
    let mut absolute = false;
    for part in [path, base] {
        if part.is_empty() || absolute {
            continue;
        }
        tail = format!("{part}/{tail}");
        absolute = part.starts_with('/');
    }
    let tail = normalize(&tail, !absolute, '/', |c| c == '/');
    if absolute {
        format!("/{tail}")
    } else if tail.is_empty() {
        ".".to_owned()
    } else {
        tail
    }
}

/// Node's `normalizeString`: drops empty and `.` segments and applies `..`.
fn normalize(
    path: &str,
    above_root: bool,
    separator: char,
    is_separator: fn(char) -> bool,
) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split(is_separator) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|last| *last != "..") {
                    segments.pop();
                } else if above_root {
                    segments.push("..");
                }
            }
            _ => segments.push(segment),
        }
    }
    segments.join(&separator.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_node_win32() {
        for [base, path, expected] in [
            ["C:\\proj", "./docs", "C:\\proj\\docs"],
            ["C:\\proj", "docs\\a", "C:\\proj\\docs\\a"],
            ["C:\\proj", "../x", "C:\\x"],
            ["C:\\proj", "..\\..\\..\\x", "C:\\x"],
            ["C:\\proj", "D:\\data", "D:\\data"],
            ["C:\\proj", "d:/data/./x/", "d:\\data\\x"],
            ["c:\\proj", "x", "c:\\proj\\x"],
            ["C:\\proj", "\\rooted", "C:\\rooted"],
            ["C:\\proj", "/rooted/a", "C:\\rooted\\a"],
            ["C:\\proj", "\\\\server\\share\\x", "\\\\server\\share\\x"],
            ["C:\\proj", "//server/share/x/../y", "\\\\server\\share\\y"],
            ["C:\\proj", "C:rel", "C:\\proj\\rel"],
            ["C:\\proj", ".", "C:\\proj"],
            ["C:\\proj\\", "a//b///c", "C:\\proj\\a\\b\\c"],
            ["C:\\", "..", "C:\\"],
            ["C:\\proj", "日本語/フォルダ", "C:\\proj\\日本語\\フォルダ"],
            ["\\\\srv\\sh\\p", "..\\..\\x", "\\\\srv\\sh\\x"],
            ["C:\\proj", "a/./b/../c", "C:\\proj\\a\\c"],
            ["C:\\proj", "node", "C:\\proj\\node"],
            ["C:\\proj", "", "C:\\proj"],
        ] {
            assert_eq!(win32_resolve(base, path), expected, "{base} + {path}");
        }
    }

    #[test]
    fn matches_node_posix() {
        for [base, path, expected] in [
            ["/proj", "./docs", "/proj/docs"],
            ["/proj", "../x", "/x"],
            ["/proj", "../../../x", "/x"],
            ["/proj", "/abs/./y/", "/abs/y"],
            ["/proj/", "a//b", "/proj/a/b"],
            ["/", "..", "/"],
            ["/proj", ".", "/proj"],
            ["/proj", "a/./b/../c", "/proj/a/c"],
            ["/proj", "C:\\x", "/proj/C:\\x"],
            ["/proj", "", "/proj"],
        ] {
            assert_eq!(posix_resolve(base, path), expected, "{base} + {path}");
        }
    }
}
