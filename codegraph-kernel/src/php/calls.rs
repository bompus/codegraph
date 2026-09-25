//! Calls, instantiations, static member references, inheritance and type references.

use super::*;

impl<'t> Walker<'t> {
    pub(super) fn extract_call(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        let caller = self.top_row();
        let mut callee_name = String::new();

        let name_field = node.child_by_field_name("name");
        let object_field = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("scope"));

        if let (Some(name_field), Some(object_field)) = (name_field, object_field) {
            // member_call_expression / scoped_call_expression.
            let method_name = self.text(name_field);

            // Fluent static-factory `Cls::factory($x)->method()` — encode
            // `Cls::factory().method` (inner args dropped) and return; the
            // inner scoped call is also visited by recursion (`Cls.factory`).
            if !method_name.is_empty() && object_field.kind() == "scoped_call_expression" {
                let inner_scope = object_field.child_by_field_name("scope");
                let inner_name = object_field.child_by_field_name("name");
                let callee = match (inner_scope, inner_name) {
                    (Some(s), Some(n)) => {
                        format!("{}::{}().{method_name}", self.text(s), self.text(n))
                    }
                    _ => method_name.to_string(),
                };
                if !callee.is_empty() {
                    self.push_ref_at(caller, &callee, edge_kind_index("calls").unwrap(), node);
                }
                return;
            }

            // receiverName = raw receiver text with ONE leading `$` stripped:
            // `$this->prop->m()` → `this->prop.m` (#1251 encoding — the whole
            // resolution machinery is TS-side); chains keep args
            // (`this->factory($cfg).m`); literals are NOT suppressed
            // (`"chain".upper`); scoped calls are DOT-joined (`UserModel.query`).
            let receiver_raw = self.text(object_field);
            let receiver = receiver_raw.strip_prefix('$').unwrap_or(receiver_raw);
            if !method_name.is_empty() {
                if matches!(receiver, "self" | "this" | "cls" | "super" | "parent" | "static") {
                    callee_name = method_name.to_string();
                } else {
                    callee_name = format!("{receiver}.{method_name}");
                }
            }
        } else {
            // function_call_expression: raw func text — bare `helper`,
            // qualified `\App\Helpers\format_id` verbatim, `$fn` for
            // variable callees, FCC `f(...)` → `f`.
            let func = node
                .child_by_field_name("function")
                .or_else(|| node.named_child(0));
            if let Some(func) = func {
                callee_name = self.text(func).to_string();
            }
        }

        if !callee_name.is_empty() {
            if let Some(c) = util::paren_conversion().captures(&callee_name) {
                callee_name = c[1].to_string();
            }
            self.push_ref_at(caller, &callee_name, edge_kind_index("calls").unwrap(), node);
        }
    }

    pub(super) fn extract_instantiation(&mut self, node: Node<'t>) {
        if self.stack.is_empty() {
            return;
        }
        // php has no constructor/type/name FIELDS → namedChild(0). Backslashes
        // are NOT split by the suffix logic → `new \App\Models\User()` keeps
        // the full qualified text; `new $cls()` keeps the `$`; an
        // anonymous_class yields its whole source text through the shared
        // normalization (garbage, deterministic — preserve).
        let ctor = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };
        let class_name = strip_generic_and_qualifier(self.text(ctor));
        if !class_name.is_empty() {
            let from = self.top_row();
            self.push_ref_at(from, &class_name, edge_kind_index("instantiates").unwrap(), node);
        }
    }

    /// extractAnonymousClass — unreachable on v0.24.2 (the declaration_list
    /// nests inside `anonymous_class`, so findAnonymousClassBody finds no
    /// DIRECT child) — mirrored from the shared TS path for shape.
    pub(super) fn extract_anonymous_class(&mut self, node: Node<'t>, body: Node<'t>) {
        stack_guard!();
        let type_node = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0));
        let mut type_name =
            type_node.map(|t| self.text(t).to_string()).unwrap_or_else(|| "Object".to_string());
        type_name = strip_generic_and_qualifier(&type_name);
        if type_name.is_empty() {
            type_name = "Object".to_string();
        }
        let anon_name = format!("<{type_name}$anon@{}>", node.start_position().row + 1);
        let Some(row) = self.create_node("class", &anon_name, node, Extra::default()) else {
            return;
        };
        let (line, column) = match type_node {
            Some(t) => (t.start_position().row as u32, self.col_of(t)),
            None => (node.start_position().row as u32, self.col_of(node)),
        };
        self.push_ref(row, &type_name, edge_kind_index("extends").unwrap(), line, column);
        self.stack.push(Scope { row, kind: "class", name: anon_name });
        for i in 0..body.named_child_count() {
            if let Some(c) = body.named_child(i) {
                self.visit_node(c);
            }
        }
        self.stack.pop();
    }

    /// extractStaticMemberRef — php's class_constant_access_expression +
    /// scoped_property_access_expression (member_access_expression is
    /// evaluated but its variable_name receiver never passes).
    pub(super) fn extract_static_member_ref(&mut self, node: Node<'t>) {
        if !matches!(
            node.kind(),
            "class_constant_access_expression" | "scoped_property_access_expression"
                | "member_access_expression"
        ) {
            return;
        }
        if self.stack.is_empty() {
            return;
        }
        let owner = self.top_row();
        if let Some(parent) = node.parent() {
            if matches!(
                parent.kind(),
                "function_call_expression" | "member_call_expression" | "scoped_call_expression"
            ) {
                let callee = parent
                    .child_by_field_name("function")
                    .or_else(|| parent.child_by_field_name("method"))
                    .or_else(|| parent.named_child(0));
                if let Some(callee) = callee {
                    if callee.start_byte() == node.start_byte() {
                        return;
                    }
                }
            }
        }
        let recv = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("expression"))
            .or_else(|| node.child_by_field_name("scope"))
            .or_else(|| node.named_child(0));
        let Some(recv) = recv else { return };
        if matches!(
            recv.kind(),
            "identifier" | "type_identifier" | "simple_identifier" | "name" | "scoped_type_identifier"
        ) {
            let text = self.text(recv);
            if capitalized_re().is_match(text) {
                self.push_ref_at(owner, text, edge_kind_index("references").unwrap(), recv);
            }
        }
    }

    /// extractInheritance — base_clause takes ONLY the first base (interface
    /// multi-extends drops the rest); class_interface_clause takes ALL
    /// children unfiltered (full text, incl. leading `\`).
    pub(super) fn extract_inheritance(&mut self, node: Node<'t>, class_row: u32) {
        let extends_kind = edge_kind_index("extends").unwrap();
        let implements_kind = edge_kind_index("implements").unwrap();
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            if child.kind() == "base_clause" {
                if let Some(target) = child.named_child(0) {
                    let name = self.text(target).to_string();
                    self.push_ref_at(class_row, &name, extends_kind, target);
                }
            } else if child.kind() == "class_interface_clause" {
                for j in 0..child.named_child_count() {
                    let Some(iface) = child.named_child(j) else { continue };
                    let name = self.text(iface).to_string();
                    self.push_ref_at(class_row, &name, implements_kind, iface);
                }
            }
        }
    }

    pub(super) fn extract_php_type_refs(&mut self, node: Node<'t>, from_row: u32) {
        let params = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "formal_parameters");
        if let Some(params) = params {
            for i in 0..params.named_child_count() {
                let Some(p) = params.named_child(i) else { continue };
                for j in 0..p.named_child_count() {
                    let Some(c) = p.named_child(j) else { continue };
                    if is_php_type_node(c.kind()) {
                        self.walk_php_type_position(c, from_row);
                    }
                }
            }
        }
        for i in 0..node.named_child_count() {
            let Some(c) = node.named_child(i) else { continue };
            if is_php_type_node(c.kind()) {
                self.walk_php_type_position(c, from_row);
            }
        }
    }

    pub(super) fn walk_php_type_position(&mut self, node: Node<'t>, from_row: u32) {
        stack_guard!();
        match node.kind() {
            "primitive_type" => {}
            "name" => {
                let name = self.text(node);
                if !name.is_empty() && !is_php_pseudo_type(name) {
                    self.push_ref_at(from_row, name, edge_kind_index("references").unwrap(), node);
                }
            }
            "qualified_name" => {
                let text = self.text(node);
                let last = text.rsplit('\\').next().unwrap_or("");
                if !last.is_empty() && !is_php_pseudo_type(last) {
                    self.push_ref_at(from_row, last, edge_kind_index("references").unwrap(), node);
                }
            }
            _ => {
                for i in 0..node.named_child_count() {
                    if let Some(c) = node.named_child(i) {
                        self.walk_php_type_position(c, from_row);
                    }
                }
            }
        }
    }
}
