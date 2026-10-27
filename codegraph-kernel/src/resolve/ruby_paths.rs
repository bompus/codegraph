//! matchRubyConstantPathRef (upstream src/resolution/ruby-constant-paths.ts)
//! and the Ruby class/instance method split (upstream #2834).
//!
//! A namespaced Ruby constant — `Billing::ChargeCard`, `::Foo`,
//! `::Billing::ChargeCard` — resolves only to a constant the project itself
//! declares, looked up from the lexical namespace around the reference
//! outward and then at top level (an absolute `::` path searches top level
//! only). A call on that path resolves a method of the class or module the
//! constant names, through its Ruby ancestry. A path the project does not
//! declare, or a method it does not have, resolves to nothing rather than
//! falling back to a same-name guess (#2827, fixing #2503).
//!
//! `def self.m` and `def m` share `Owner::m` in the index; `is_static`
//! distinguishes them. A call on a class constant prefers the class
//! method, one on a receiver `x = Widget.new` prefers the instance method,
//! and a bare call prefers the kind of the method it is written inside —
//! but only when both kinds really exist under the same qualified name,
//! never when the receiver is just uncertain (#2834).

use super::*;

impl KernelResolver {
    /// matchRubyConstantPathRef: `references` names that are constant paths
    /// resolve to the constant; `calls` names `Path.method` resolve to the
    /// owner's method. Returns `Some` (resolved or unresolved) whenever the
    /// ref spells a constant path — the claim is exclusive — and `None` for
    /// every other ref.
    pub(super) fn resolve_ruby_constant_path_ref(&mut self, r: &ResolveRefIn) -> Res<Option<ResolveOutcome>> {
        if r.language != "ruby" {
            return Ok(None);
        }
        // A bare `Foo` or `Foo.call` is left to the name strategies; only a
        // path written with its namespace (`Foo::Bar`, `::Foo`) is claimed.
        let namespaced = |c: &regex::Captures| c.get(1).is_some() || c[2].contains("::");
        if r.reference_kind == "references" {
            let Some(c) = re!(r"^(::)?([A-Z]\w*(?:::[A-Z]\w*)*)$").captures(r.reference_name.trim_start_matches("()")) else {
                return Ok(None);
            };
            if !namespaced(&c) {
                return Ok(None);
            }
            return match self.ruby_constants_named(&c[2], r, c.get(1).is_some())?.into_iter().next() {
                Some(target) => Ok(Some(ResolveOutcome::resolved(&target, 0.9, "qualified-name", true, None))),
                None => Ok(Some(ResolveOutcome::unresolved())),
            };
        }
        if r.reference_kind == "calls" {
            let Some(c) = re!(r"^(::)?([A-Z]\w*(?:::[A-Z]\w*)*)\.([A-Za-z_]\w*[?!=]?)$").captures(r.reference_name.trim_start_matches("()")) else {
                return Ok(None);
            };
            if !namespaced(&c) {
                return Ok(None);
            }
            let owner = self
                .ruby_constants_named(&c[2], r, c.get(1).is_some())?
                .into_iter()
                .find(|n| matches!(n.kind.as_str(), "class" | "module"));
            let Some(owner) = owner else {
                return Ok(Some(ResolveOutcome::unresolved()));
            };
            return match self.ruby_method_of(&owner.qualified_name, &c[3], r)? {
                Some((method, own)) => Ok(Some(ResolveOutcome::resolved(
                    &method,
                    if own { 0.85 } else { 0.8 },
                    "qualified-name",
                    true,
                    None,
                ))),
                None => Ok(Some(ResolveOutcome::unresolved())),
            };
        }
        Ok(None)
    }

    /// constantsNamed: the declarations of the first of `<scope>::<path>`,
    /// `<outer>::<path>`, … `<path>` the project declares — the scope being
    /// the class or module around the reference, `<path>` alone for a
    /// `::`-rooted name. Production code never names a constant only a test
    /// declares.
    fn ruby_constants_named(&mut self, path: &str, r: &ResolveRefIn, absolute: bool) -> Res<Vec<Arc<KNode>>> {
        let production = !super::resolver_upstream::test_suite_path(&r.file_path);
        let mut prefix = if absolute { String::new() } else { self.ruby_lexical_scope(r)? };
        loop {
            let qn = if prefix.is_empty() { path.to_string() } else { format!("{prefix}::{path}") };
            let found: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(&qn)?
                .iter()
                .filter(|n| {
                    n.language == "ruby"
                        && matches!(n.kind.as_str(), "class" | "module" | "variable" | "constant")
                        && !(production && super::resolver_upstream::test_suite_path(&n.file_path))
                })
                .cloned()
                .collect();
            if !found.is_empty() {
                return Ok(ruby_ordered(found, r));
            }
            if prefix.is_empty() {
                return Ok(Vec::new());
            }
            prefix = prefix.rfind("::").map_or(String::new(), |i| prefix[..i].to_string());
        }
    }

    /// lexicalScope: the qualified name of the innermost class or module
    /// around `r`, or empty at the top level of its file.
    fn ruby_lexical_scope(&mut self, r: &ResolveRefIn) -> Res<String> {
        let mut inner: Option<Arc<KNode>> = None;
        for n in self.nodes_in_file(&r.file_path)?.iter() {
            if n.language != "ruby" || !matches!(n.kind.as_str(), "class" | "module") || n.start_line > r.line || n.end_line < r.line {
                continue;
            }
            if inner.as_ref().is_none_or(|i| n.start_line > i.start_line || (n.start_line == i.start_line && n.start_column > i.start_column)) {
                inner = Some(n.clone());
            }
        }
        Ok(inner.map(|i| i.qualified_name.clone()).unwrap_or_default())
    }

    /// methodOf: `name` declared on `owner_qn` itself (kind `class` first),
    /// else on the first ancestor in its Ruby ancestry that declares it.
    /// Returns the method and whether it is the owner's own. `r` is the call
    /// site: production code never reaches a method only a test declares.
    pub(super) fn ruby_method_of(&mut self, owner_qn: &str, name: &str, r: &ResolveRefIn) -> Res<Option<(Arc<KNode>, bool)>> {
        let production = !super::resolver_upstream::test_suite_path(&r.file_path);
        let own: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&format!("{owner_qn}::{name}"))?
            .iter()
            .filter(|n| n.language == "ruby" && n.kind == "method" && !(production && super::resolver_upstream::test_suite_path(&n.file_path)))
            .cloned()
            .collect();
        if let Some(m) = ruby_methods_of_kind_first(ruby_ordered(own, r), "class").into_iter().next() {
            return Ok(Some((m, true)));
        }
        let ancestry = self.ruby_ancestry(owner_qn)?;
        let mut ancestors: Vec<&String> = ancestry.iter().filter(|q| q.as_str() != owner_qn).collect();
        ancestors.sort();
        for qn in ancestors {
            let found: Vec<Arc<KNode>> = self
                .nodes_by_qualified_name(&format!("{qn}::{name}"))?
                .iter()
                .filter(|n| n.language == "ruby" && n.kind == "method" && !(production && super::resolver_upstream::test_suite_path(&n.file_path)))
                .cloned()
                .collect();
            if let Some(m) = ruby_methods_of_kind_first(ruby_ordered(found, r), "class").into_iter().next() {
                return Ok(Some((m, false)));
            }
        }
        Ok(None)
    }

    /// rubyMethodsOfCallerKind: of the methods `build` could mean, the ones
    /// whose `is_static` matches the enclosing method's stay. Applies only
    /// when a qualified name holds both kinds and the caller is itself a
    /// Ruby method — a bare call from a block or the file's top level keeps
    /// every candidate.
    pub(super) fn ruby_methods_of_caller_kind(&mut self, methods: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Res<Vec<Arc<KNode>>> {
        let mut static_qns = HashSet::new();
        let mut instance_qns = HashSet::new();
        for m in methods.iter().filter(|m| m.language == "ruby" && m.kind == "method") {
            if m.is_static {
                static_qns.insert(m.qualified_name.clone());
            } else {
                instance_qns.insert(m.qualified_name.clone());
            }
        }
        let twins: HashSet<String> = static_qns.intersection(&instance_qns).cloned().collect();
        if twins.is_empty() {
            return Ok(methods);
        }
        let caller_static = match self.node_by_id(&r.from_node_id)? {
            Some(c) if c.language == "ruby" && c.kind == "method" => c.is_static,
            _ => return Ok(methods),
        };
        Ok(methods
            .into_iter()
            .filter(|m| !(m.language == "ruby" && m.kind == "method" && twins.contains(&m.qualified_name) && m.is_static != caller_static))
            .collect())
    }
}

/// `ordered`: anything before a value, then the reference's own file, then
/// production code; the index's order within each.
fn ruby_ordered(mut nodes: Vec<Arc<KNode>>, r: &ResolveRefIn) -> Vec<Arc<KNode>> {
    if nodes.len() < 2 {
        return nodes;
    }
    let rank = |n: &KNode| {
        matches!(n.kind.as_str(), "variable" | "constant") as u8 * 4
            + (n.file_path != r.file_path) as u8 * 2
            + super::resolver_upstream::test_suite_path(&n.file_path) as u8
    };
    nodes.sort_by_key(|n| rank(n));
    nodes
}

/// rubyMethodsOfKindFirst: methods whose `is_static` matches the wanted
/// kind, then the rest in their original order. `kind` is `"class"` for
/// calls on a class or module constant and `"instance"` for calls on a
/// receiver bound to an instance.
pub(super) fn ruby_methods_of_kind_first(methods: Vec<Arc<KNode>>, kind: &str) -> Vec<Arc<KNode>> {
    let want = kind == "class";
    let (mut same, other): (Vec<Arc<KNode>>, Vec<Arc<KNode>>) = methods.into_iter().partition(|m| m.is_static == want);
    same.extend(other);
    same
}
