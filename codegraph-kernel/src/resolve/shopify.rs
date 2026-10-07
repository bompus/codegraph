//! Liquid section and snippet references stay inside their nearest Shopify theme.
use super::*;

impl KernelResolver {
    pub(super) fn resolve_shopify_file(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if r.language != "liquid" || !(r.reference_name.starts_with("sections/") || r.reference_name.starts_with("snippets/")) { return Ok(None); }
        let parts: Vec<_> = r.file_path.split('/').collect();
        for i in (0..parts.len().saturating_sub(1)).rev() {
            if !matches!(parts[i].to_ascii_lowercase().as_str(), "assets" | "blocks" | "config" | "layout" | "locales" | "sections" | "snippets" | "templates") { continue; }
            let root = parts[..i].join("/");
            let prefix = if root.is_empty() { String::new() } else { format!("{root}/") };
            if !["layout/theme.liquid", "config/settings_schema.json"].iter().any(|marker| self.file_exists(&format!("{prefix}{marker}"))) { continue; }
            let target = format!("{prefix}{}", r.reference_name);
            let node = self.nodes_in_file(&target)?.iter().find(|n| n.kind == "file").cloned();
            return match node {
                Some(node) => self.finish_pre_framework(r, KCand { node, confidence: if root.is_empty() { 0.95 } else { 0.85 }, resolved_by: "file-path" }).map(Some),
                None => Ok(Some(ResolveOutcome::unresolved())),
            };
        }
        Ok(None)
    }
}
