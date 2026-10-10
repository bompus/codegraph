//! A Go local bound from a type assertion has the asserted type:
//!
//! ```go
//! if f, ok := w.(http.Flusher); ok { f.Flush() }   // http.Flusher's, nothing in the project
//! wr := v.(*Wrapper)                               // wr.Flush() is what *Wrapper has
//! ```
//!
//! The local's declaration is read from the syntax tree, so the binding is the
//! one in scope at the call: an `if` header's variable does not reach a later
//! variable of that name, a block's shadowing declaration ends with the block,
//! and a comment declares nothing. A parameter, a range or type-switch
//! variable, or a `:=` whose value is not just the assertion declares the
//! name some other way and is left to the other receiver inference.

use super::*;
use tree_sitter::Node as TsNode;

/// What a Go type written at a call site names.
pub(super) enum GoAsserted {
    /// A type the project declares, with the directory of its package.
    Project { name: String, dir: String },
    /// A type from outside the project or a predeclared one: no method the
    /// project declares.
    Outside,
}

/// How a name in scope is declared.
enum Declared {
    Asserted(String),
    Other,
}

fn text_of<'a>(node: TsNode, text: &'a str) -> &'a str {
    &text[node.start_byte()..node.end_byte()]
}

/// The value of `names = values` when it is one type assertion, for the name
/// at `index` of `names` (the asserted value goes to the first name).
fn declared_by(names: &[TsNode], values: Option<TsNode>, typed: bool, name: &str, text: &str) -> Option<Declared> {
    let index = names.iter().position(|n| text_of(*n, text) == name)?;
    let asserted = (|| {
        if typed || index != 0 { return None; }
        let values = values?;
        let mut cursor = values.walk();
        let items: Vec<_> = values.named_children(&mut cursor).collect();
        let [only] = items.as_slice() else { return None; };
        if only.kind() != "type_assertion_expression" { return None; }
        only.child_by_field_name("type").map(|t| text_of(t, text).to_string())
    })();
    Some(asserted.map_or(Declared::Other, Declared::Asserted))
}

fn identifiers<'t>(list: TsNode<'t>) -> Vec<TsNode<'t>> {
    if list.kind() == "identifier" { return vec![list]; }
    let mut cursor = list.walk();
    list.named_children(&mut cursor).filter(|n| n.kind() == "identifier").collect()
}

/// `name`'s declaration among one statement, if the statement declares it.
fn statement_declares(stmt: TsNode, name: &str, text: &str) -> Option<Declared> {
    match stmt.kind() {
        "short_var_declaration" => {
            let left = identifiers(stmt.child_by_field_name("left")?);
            declared_by(&left, stmt.child_by_field_name("right"), false, name, text)
        }
        "var_declaration" | "const_declaration" | "var_spec_list" => {
            let mut cursor = stmt.walk();
            let mut found = None;
            for spec in stmt.named_children(&mut cursor) {
                let hit = if spec.kind() == "var_spec_list" { statement_declares(spec, name, text) } else if matches!(spec.kind(), "var_spec" | "const_spec") {
                    let mut c = spec.walk();
                    let names: Vec<_> = spec.children_by_field_name("name", &mut c).collect();
                    let typed = spec.child_by_field_name("type").is_some() || spec.kind() == "const_spec";
                    declared_by(&names, spec.child_by_field_name("value"), typed, name, text)
                } else { None };
                if hit.is_some() { found = hit; }
            }
            found
        }
        _ => None,
    }
}

fn params_declare(list: Option<TsNode>, name: &str, text: &str) -> bool {
    let Some(list) = list else { return false; };
    let mut cursor = list.walk();
    for param in list.named_children(&mut cursor) {
        let mut c = param.walk();
        if param.children_by_field_name("name", &mut c).any(|n| text_of(n, text) == name) { return true; }
    }
    false
}

/// The declaration of `name` that `scope` itself holds before `site`, if any.
fn scope_declares(scope: TsNode, name: &str, site: usize, text: &str) -> Option<Declared> {
    match scope.kind() {
        "block" | "expression_case" | "default_case" | "type_case" | "communication_case" => {
            let mut cursor = scope.walk();
            let mut found = None;
            for stmt in scope.named_children(&mut cursor).filter(|s| s.end_byte() <= site) {
                if let Some(hit) = statement_declares(stmt, name, text) { found = Some(hit); }
            }
            found
        }
        "if_statement" | "expression_switch_statement" | "type_switch_statement" | "for_statement" => {
            let mut found = None;
            if let Some(init) = scope.child_by_field_name("initializer").filter(|i| i.end_byte() <= site) {
                found = statement_declares(init, name, text);
            }
            if scope.kind() == "type_switch_statement" {
                if let Some(alias) = scope.child_by_field_name("alias").filter(|a| a.end_byte() <= site) {
                    if identifiers(alias).iter().any(|n| text_of(*n, text) == name) { found = Some(Declared::Other); }
                }
            }
            if scope.kind() == "for_statement" {
                let mut cursor = scope.walk();
                for clause in scope.named_children(&mut cursor).filter(|c| c.end_byte() <= site) {
                    match clause.kind() {
                        "range_clause" => {
                            if let Some(left) = clause.child_by_field_name("left") {
                                if identifiers(left).iter().any(|n| text_of(*n, text) == name) { found = Some(Declared::Other); }
                            }
                        }
                        "for_clause" => {
                            if let Some(hit) = clause.child_by_field_name("initializer").and_then(|i| statement_declares(i, name, text)) { found = Some(hit); }
                        }
                        _ => {}
                    }
                }
            }
            found
        }
        "function_declaration" | "method_declaration" | "func_literal" => {
            let declared = params_declare(scope.child_by_field_name("parameters"), name, text)
                || params_declare(scope.child_by_field_name("receiver"), name, text)
                || params_declare(scope.child_by_field_name("result").filter(|r| r.kind() == "parameter_list"), name, text);
            declared.then_some(Declared::Other)
        }
        _ => None,
    }
}

impl KernelResolver {
    /// The type a Go local was asserted to, as written, when the declaration
    /// of `name` in scope at `r` binds the value of a type assertion.
    pub(super) fn go_asserted_local_type(&mut self, name: &str, r: &ResolveRefIn) -> Option<String> {
        if r.language != "go" || name.is_empty() || !name.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric()) { return None; }
        let file = self.read_file(&r.file_path)?;
        let text = file.text();
        // Only a file that asserts a type can bind from one.
        if !text.contains(".(") { return None; }
        let tree = self.parsed_tree(&file, r)?;
        let site = super::iteration::descendant_for_position(tree.root_node(), text,
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize + 1));
        let at = site.start_byte();
        let mut scope = Some(site);
        while let Some(node) = scope {
            if let Some(declared) = scope_declares(node, name, at, text) {
                return match declared { Declared::Asserted(ty) => Some(ty), Declared::Other => None };
            }
            if matches!(node.kind(), "function_declaration" | "method_declaration") { return None; }
            scope = node.parent();
        }
        None
    }

    /// The project type a Go type written at `r` names, found where Go finds
    /// it: in the file's own package or a package it dot-imports for a bare
    /// name, in the imported package for a qualified one. `None` for a type
    /// the index cannot tell (a type literal, a type declared inside a
    /// function, a type parameter): the call goes to the other strategies.
    pub(super) fn go_written_type(&mut self, written: &str, r: &ResolveRefIn) -> Res<Option<GoAsserted>> {
        let Some(hit) = re!(r"^\*?\s*(?:([A-Za-z_]\w*)\s*\.\s*)?([A-Za-z_]\w*)\s*(?:\[[\s\S]*\])?$").captures(written.trim()) else { return Ok(None); };
        let qualifier = hit.get(1).map(|m| m.as_str().to_string());
        let name = hit[2].to_string();
        let declared = self.nodes_by_name(&name)?.iter()
            .filter(|n| n.language == "go" && matches!(n.kind.as_str(), "struct" | "interface" | "type_alias") && !n.qualified_name.contains("::"))
            .map(|n| pos_dirname(&n.file_path).to_string()).collect::<HashSet<_>>();
        let dir = if let Some(qualifier) = qualifier {
            match self.go_imported_package_dir(&r.file_path, &qualifier)? {
                None => return Ok(Some(GoAsserted::Outside)),
                Some(dir) if declared.contains(&dir) => dir,
                Some(_) => return Ok(None),
            }
        } else {
            let mut dirs = vec![pos_dirname(&r.file_path).to_string()];
            for import in self.import_mappings(&r.file_path)?.iter().filter(|i| i.local_name == ".") {
                if let Some(dir) = self.go_package_dir(&import.source, &r.file_path) { dirs.push(dir); }
            }
            match dirs.into_iter().find(|d| declared.contains(d)) {
                Some(dir) => dir,
                None => return Ok(GO_BUILTIN_FIELD_TYPES.contains(name.as_str()).then_some(GoAsserted::Outside)),
            }
        };
        Ok(Some(GoAsserted::Project { name, dir }))
    }

    /// The type of the Go local `name` at `r` when it is bound from a type
    /// assertion whose type is settled. `None`: not such a local.
    pub(super) fn go_asserted_local(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<GoAsserted>> {
        let Some(written) = self.go_asserted_local_type(name, r) else { return Ok(None); };
        self.go_written_type(&written, r)
    }

    /// The verdict for `receiver.method` when the receiver is an asserted
    /// local: the method the asserted type has, or nothing. `None`: not an
    /// asserted local; the other strategies decide.
    pub(super) fn go_asserted_member(&mut self, receiver: &str, method: &str, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        Ok(match self.go_asserted_local(receiver, r)? {
            None => None,
            Some(GoAsserted::Outside) => Some(None),
            Some(GoAsserted::Project { name, dir }) => Some(self.go_method_in_package(&name, method, &dir, r)?),
        })
    }
}
