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
use super::fields::GoAliasTarget;
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
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
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
            let Some(dir) = self.go_bare_type_dir(&name, &r.file_path)? else {
                return Ok(Some(GoAsserted::Outside));
            };
            if !declared.contains(&dir) {
                return Ok(GO_BUILTIN_FIELD_TYPES.contains(name.as_str()).then_some(GoAsserted::Outside));
            }
            dir
        };
        // An alias of a type from outside the project (`type Ctx =
        // context.Context`) has that type's methods, none the project
        // declares. Every declaration an outside alias means none here.
        let types = self.go_package_types(&name, &dir)?;
        if !types.is_empty() {
            let mut all_outside_alias = true;
            for t in &types {
                if !matches!(self.go_alias_target(t)?, GoAliasTarget::Null) {
                    all_outside_alias = false;
                    break;
                }
            }
            if all_outside_alias {
                return Ok(Some(GoAsserted::Outside));
            }
        }
        Ok(Some(GoAsserted::Project { name, dir }))
    }

    /// The type of the Go local `name` at `r` when it is bound from a type
    /// assertion whose type is settled. `None`: not such a local.
    pub(super) fn go_asserted_local(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<GoAsserted>> {
        let Some(written) = self.go_asserted_local_type(name, r) else { return Ok(None); };
        self.go_written_type(&written, r)
    }

    /// The verdict for `receiver.method` when the receiver is an asserted
    /// local: the method the asserted type has, or nothing — a miss and a
    /// type the index cannot read are both terminal; the fork never guesses
    /// a callee for an asserted receiver by name. `None`: not an asserted
    /// local; the other strategies decide.
    pub(super) fn go_asserted_member(&mut self, receiver: &str, method: &str, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        let Some(written) = self.go_asserted_local_type(receiver, r) else { return Ok(None); };
        Ok(Some(match self.go_written_type(&written, r)? {
            Some(GoAsserted::Outside) => None,
            Some(GoAsserted::Project { name, dir }) => self.go_method_in_package(&name, method, &dir, r)?,
            None => {
                // An exported type the file declares — `type T interface`
                // inside the function — is no index node; its call goes to
                // the name strategies, as before. A literal, an unexported
                // or an unknown name stays refused: the fork never guesses
                // a callee for an asserted receiver it cannot type.
                let bare = written.trim_start_matches('*').rsplit('.').next().unwrap_or("");
                let local = Self::go_named_type(&written).is_some()
                    && bare.chars().next().is_some_and(|c| c.is_uppercase())
                    && self.go_bare_type_dir(bare, &r.file_path)?.is_some();
                if local { self.go_mc_untyped_local = true; return Ok(None); }
                None
            }
        }))
    }
}

/// An actual lexical declaration, including the name's position for reading
/// its initializer. Used by func-field synthesis through the native bridge.
fn local_declaration_in<'a>(scope: TsNode<'a>, name: &str, site: usize, text: &str) -> Option<(TsNode<'a>, Option<String>)> {
    /// `x := &pkg.T{…}`, `x := T{…}`, `x := new(T)` and `var x = &T{…}`
    /// write their type in the value; other right-hand sides say nothing.
    /// `k` is the name's place among the declaration's names — the value
    /// list's same place is its initializer.
    fn value_type<'a>(value: Option<TsNode<'a>>, k: usize, text: &'a str) -> Option<String> {
        let mut v = value?;
        if v.kind() == "expression_list" {
            v = if v.named_child_count() > k { v.named_child(k)? } else { v.named_child(v.named_child_count() - 1)? };
        }
        while v.kind() == "parenthesized_expression" {
            v = v.named_child(0)?;
        }
        if v.kind() == "unary_expression" {
            if text.get(v.start_byte()..v.start_byte() + 1)? != "&" { return None; }
            v = v.named_child(0)?;
        }
        match v.kind() {
            "composite_literal" => v.child_by_field_name("type").map(|t| text_of(t, text).to_string()),
            // A type switch's `x := v.(type)` binds a per-clause type the
            // declaration does not spell: unreadable, never a project type.
            "type_assertion_expression" => Some(match v.child_by_field_name("type") {
                Some(t) if text_of(t, text) != "type" => text_of(t, text).to_string(),
                _ => "<type>".to_string(),
            }),
            "call_expression" => {
                let function = v.child_by_field_name("function")?;
                if function.kind() != "identifier" || !matches!(text_of(function, text), "new" | "make") { return None; }
                v.child_by_field_name("arguments")?.named_child(0).map(|t| text_of(t, text).to_string())
            }
            _ => None,
        }
    }
    fn statement<'a>(stmt: TsNode<'a>, name: &str, text: &str) -> Option<(TsNode<'a>, Option<String>)> {
        match stmt.kind() {
            "short_var_declaration" | "range_clause" => {
                let names = identifiers(stmt.child_by_field_name("left")?);
                let (k, target) = names.iter().enumerate().find(|(_, n)| text_of(**n, text) == name).map(|(k, n)| (k, *n))?;
                Some((target, value_type(stmt.child_by_field_name("right"), k, text)))
            }
            "var_spec" | "const_spec" | "parameter_declaration" | "variadic_parameter_declaration" => {
                let mut c = stmt.walk();
                let names: Vec<_> = stmt.children_by_field_name("name", &mut c).collect();
                let (k, target) = names.iter().enumerate().find(|(_, n)| text_of(**n, text) == name).map(|(k, n)| (k, *n))?;
                let declared = stmt.child_by_field_name("type").map(|t| text_of(t, text).to_string());
                Some((target, declared.or_else(|| value_type(stmt.child_by_field_name("value"), k, text))))
            }
            "var_declaration" | "const_declaration" | "var_spec_list" | "parameter_list" => {
                let mut c = stmt.walk();
                stmt.named_children(&mut c).filter_map(|s| statement(s, name, text)).last()
            }
            _ => None,
        }
    }
    match scope.kind() {
        "block" | "expression_case" | "default_case" | "type_case" | "communication_case" => {
            let mut c=scope.walk();
            scope.named_children(&mut c).filter(|s| s.end_byte() <= site).filter_map(|s| statement(s, name, text)).last()
        }
        "if_statement" | "expression_switch_statement" | "type_switch_statement" | "for_statement" => {
            if let Some(alias) = scope.child_by_field_name("alias").filter(|n| n.end_byte() <= site) {
                let decl = if alias.kind() == "short_var_declaration" { Some(alias) } else {
                    let mut c = alias.walk();
                    let mut it = alias.named_children(&mut c);
                    it.find(|n| n.kind() == "short_var_declaration")
                };
                if let Some(d) = decl {
                    let names = identifiers(d.child_by_field_name("left")?);
                    if let Some((k, target)) = names.iter().enumerate().find(|(_, n)| text_of(**n, text) == name).map(|(k, n)| (k, *n)) {
                        return Some((target, value_type(d.child_by_field_name("right"), k, text)));
                    }
                }
            }
            let mut c=scope.walk();
            for child in scope.named_children(&mut c).filter(|n| n.end_byte() <= site) {
                if let Some(hit) = statement(child, name, text) { return Some(hit); }
                if child.kind() == "for_clause" {
                    if let Some(hit) = child.child_by_field_name("initializer").and_then(|i| statement(i,name,text)) { return Some(hit); }
                }
            }
            None
        }
        "function_declaration" | "method_declaration" | "func_literal" => {
            ["receiver", "parameters", "result"].iter().filter_map(|f| scope.child_by_field_name(f)).filter(|n| n.kind() == "parameter_list").find_map(|n| statement(n,name,text))
        }
        _ => None,
    }
}

#[cfg_attr(feature = "addon", napi(object))]
pub struct GoLocalDeclaration { pub line: i64, pub column: i64, pub declared_type: Option<String> }

/// Source-only lexical lookup. Synthesis must not attach to the live WAL.
#[cfg_attr(feature = "addon", napi)]
pub struct GoSourceScope {
    source: String,
    tree: tree_sitter::Tree,
}

#[cfg_attr(feature = "addon", napi)]
impl GoSourceScope {
    #[cfg_attr(feature = "addon", napi(constructor))]
    pub fn new(source: String) -> Result<Self> {
        let tree = crate::tree::parse_with_cached_parser(&source, "go")?;
        Ok(Self { source, tree })
    }

    #[cfg_attr(feature = "addon", napi)]
    pub fn declaration_at(&self, name: String, line: i64, column: i64) -> Option<GoLocalDeclaration> {
        local_at(&self.tree, &self.source, &name, line, column)
    }
}

fn local_at(tree: &tree_sitter::Tree, source: &str, name: &str, line: i64, column: i64) -> Option<GoLocalDeclaration> {
        let site = super::iteration::descendant_for_position(tree.root_node(), source,
            ((line - 1).max(0) as usize, column.max(0) as usize));
        let mut scope = Some(site);
        while let Some(node) = scope {
            if let Some((target, ty)) = local_declaration_in(node, name, site.start_byte(), source) {
                return Some(GoLocalDeclaration { line: target.start_position().row as i64 + 1,
                    column: target.start_position().column as i64, declared_type: ty });
            }
            if matches!(node.kind(), "function_declaration" | "method_declaration") { break; }
            scope = node.parent();
        }
        None
}

impl KernelResolver {
    pub(super) fn go_local_declaration_at(&mut self, name: String, file_path: String, line: i64, column: i64) -> Result<Option<GoLocalDeclaration>> {
        let r = ResolveRefIn { row_id: None, from_node_id: String::new(), reference_name: name.clone(), reference_kind: "references".into(),
            line, column, candidates: None, file_path: file_path.clone(), language: "go".into(), failure_reason: None, generated: None };
        let Some(file) = self.read_file(&file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&file, &r) else { return Ok(None) };
        Ok(local_at(&tree, file.text(), &name, line, column))
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    fn declaration(source: &str, name: &str, needle: &str) -> Option<(usize, Option<String>)> {
        let mut parser=tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into()).unwrap();
        let tree=parser.parse(source,None).unwrap(); let offset=source.find(needle).unwrap();
        let mut scope=tree.root_node().descendant_for_byte_range(offset,offset);
        while let Some(node)=scope {
            if let Some((target,ty))=local_declaration_in(node,name,offset,source) { return Some((target.start_position().row+1,ty)); }
            scope=node.parent();
        }
        None
    }
    #[test]
    fn grouped_and_qualified_parameters_keep_their_declared_types() {
        assert_eq!(declaration("package p\nfunc f(a,b *other.Client) { b.Send() }", "b", "b.Send"),Some((2,Some("*other.Client".into()))));
    }
    #[test]
    fn closed_case_locals_and_func_result_parameters_do_not_escape() {
        let text="package p\nfunc f(x other.Client) func(hidden T) {\n switch n {\n case 1: x := v; x.Send()\n case 2: x.Send()\n }\n x.Send()\n}";
        assert_eq!(declaration(text,"x","case 2: x.Send"),Some((2,Some("other.Client".into()))));
        assert_eq!(declaration(text,"hidden","case 2:"),None);
    }
}
