//! ES-module declaration visibility and callable exports returned by factories.

use super::*;
use super::awaited::{blank_string_contents, strip_ts_comments};

impl KernelResolver {
    /// Whether `name` at this call is one of the browser's own objects rather
    /// than a project value: a JS-family name in JS_BROWSER_HOST_GLOBALS that
    /// the file writes only as a receiver (`document.x`, `history.back()`,
    /// `self?.close()`) or after `typeof`. A file that binds the name — a
    /// parameter, plain or destructured, a declaration, `var self = this`, an
    /// import — keeps it its own (#2907).
    pub(super) fn is_browser_host_global(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        if !is_js_family(&r.language) || !JS_BROWSER_HOST_GLOBALS.contains(name) {
            return Ok(false);
        }
        // A lexical binding of the name at the call (`document` as a
        // parameter, a `const history`) is the file's own, whatever the
        // template or comment blanking below can see.
        if self.receiver_binding(name, r)?.is_some_and(|b| b.kind != "import") {
            return Ok(false);
        }
        let key = format!("{}\0{}", r.file_path, name);
        if let Some(&hit) = self.js_host_receiver_only.get(&key) {
            return Ok(hit);
        }
        let only = self.uses_name_only_as_receiver(name, &r.file_path)?;
        self.js_host_receiver_only.insert(key, only);
        Ok(only)
    }

    /// Whether the JS/TS file writes `name` only as a receiver or after
    /// `typeof`. Any other mention may bind or hand over a value of the file's
    /// own. Read with comments and strings blanked; `x.document` is no mention.
    fn uses_name_only_as_receiver(&mut self, name: &str, file_path: &str) -> Res<bool> {
        let Some(src) = self.read_file(file_path) else { return Ok(true) };
        let code = blank_string_contents(&strip_ts_comments(src.text()));
        let mut only = true;
        let mut from = 0usize;
        while let Some(rel) = code[from..].find(name) {
            let at = from + rel;
            let end = at + name.len();
            from = at + 1;
            let previous = code[..at].chars().next_back();
            if previous.is_some_and(|c| is_word_char(c) || c == '.' || c == '#') { continue; }
            if code[end..].chars().next().is_some_and(is_word_char) { continue; }
            let rest = code[end..].trim_start();
            let receiver = (rest.starts_with('.') && !rest.starts_with(".."))
                || rest.starts_with('[')
                || rest.starts_with("?.");
            // A byte 16 back need not be a character boundary (`π`).
            let mut window_start = at.saturating_sub(16);
            while !code.is_char_boundary(window_start) { window_start -= 1; }
            let window = &code[window_start..at];
            let after_typeof = window.rfind("typeof").is_some_and(|i| {
                window[..i].chars().next_back().is_none_or(|c| !is_word_char(c))
                    && window[i + 6..].chars().all(char::is_whitespace)
            });
            if !receiver && !after_typeof { only = false; break; }
        }
        Ok(only)
    }

    pub(super) fn is_unexported_module_binding(&mut self, n: &KNode) -> Res<bool> {
        if !is_esm_family(&n.language) || n.is_exported || n.qualified_name.contains("::")
            || !matches!(n.kind.as_str(), "function" | "variable" | "constant" | "class" | "interface" | "type_alias" | "enum" | "component")
            || re!(r"\.d\.[cm]?ts$").is_match(&n.file_path) {
            return Ok(false);
        }
        if !self.esm_exports_memo.contains_key(&n.file_path) {
            let Some(lines) = self.read_file(&n.file_path) else { return Ok(false) };
            let code = blank_string_contents(&strip_ts_comments(lines.text()));
            let module = re!(r"(?m)^\s*(?:import\s|export\s)").is_match(&code)
                && !re!(r"(?-u:\b)(?:module\.exports|exports\.|declare\s+global)").is_match(&code);
            let mut names = HashSet::new();
            if module {
                for m in re!(r"(?m)^[ \t]*export\s+(?:type\s+)?\{([^}]*)\}").captures_iter(&code) {
                    for item in m[1].split(',') {
                        let local = item.trim().strip_prefix("type ").unwrap_or(item.trim()).split_whitespace().next().unwrap_or("");
                        names.insert(local.to_string());
                    }
                }
                for m in re!(r"(?m)^[ \t]*export\s+(?:default|=)\s+([A-Za-z_$][\w$]*)\s*;?\s*$").captures_iter(&code) {
                    names.insert(m[1].to_string());
                }
                // `export default function Disclosure() {}` exports the
                // declaration's own name to a default import.
                for m in re!(r"(?m)^[ \t]*export\s+default\s+(?:async\s+)?(?:function|class)\s+([A-Za-z_$][\w$]*)").captures_iter(&code) {
                    names.insert(m[1].to_string());
                }
                for m in re!(r"(?m)^[ \t]*export\s+default\s*\{([^}]*)\}|(?-u:\b)return\s*\{([^{}]*)\}").captures_iter(&code) {
                    for item in m.get(1).or_else(|| m.get(2)).unwrap().as_str().split(',') {
                        let value = item.rsplit(':').next().unwrap_or("").trim();
                        if re!(r"^[A-Za-z_$][\w$]*$").is_match(value) { names.insert(value.to_string()); }
                    }
                }
            }
            self.esm_exports_memo.insert(n.file_path.clone(), (module, names));
        }
        let (module, names) = &self.esm_exports_memo[&n.file_path];
        if !module || names.contains(&n.name) { return Ok(false) }
        let Some(lines) = self.read_file(&n.file_path) else { return Ok(false) };
        let line = lines.get((n.start_line - 1).max(0) as usize).map(String::as_str).unwrap_or("");
        Ok(re!(r"^\s*(?:declare\s+)?(?:async\s+)?(?:function\*?|const|let|var|(?:abstract\s+)?class|interface|type|enum)\s").is_match(line))
    }
}

/// A JS identifier character.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}
