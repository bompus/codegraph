//! ES-module declaration visibility and callable exports returned by factories.

use super::*;
use super::awaited::{blank_string_contents, strip_ts_comments};

impl KernelResolver {
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
