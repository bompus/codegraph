//! C function-pointer typedefs and callable struct fields (the fields the fn-pointer linker dispatches through).

use super::*;

impl<'t> Walker<'t> {
    /// Register the fn-pointer typedef names a `type_definition` (C/C++) or a
    /// cpp `alias_declaration` (`using Fn = int(*)(int)`) introduces. The
    /// registries are file-local and never cleared: C requires
    /// declare-before-use, so a name registered here is visible to every
    /// aggregate declared later in the walk.
    pub(super) fn register_fn_typedefs(&mut self, node: Node<'t>) {
        if node.kind() == "alias_declaration" {
            // `using Name = <type>`: classify the type subtree — a
            // pointer inside parens of an abstract_function_declarator is a
            // fn-pointer alias; a bare abstract_function_declarator is a
            // function-type alias.
            let Some(name_node) = node.child_by_field_name("name") else { return };
            let Some(mut ty) = node.child_by_field_name("type") else { return };
            while ty.kind() == "type_descriptor" {
                let Some(d) = ty.child_by_field_name("declarator") else { return };
                ty = d;
            }
            if ty.kind() != "abstract_function_declarator" {
                return;
            }
            let mut inner = ty.child_by_field_name("declarator");
            let mut is_ptr = false;
            while let Some(i) = inner {
                match i.kind() {
                    "abstract_parenthesized_declarator" => {
                        inner = i.child_by_field_name("declarator").or_else(|| i.named_child(0));
                    }
                    "abstract_pointer_declarator" => {
                        is_ptr = true;
                        break;
                    }
                    _ => break,
                }
            }
            let name = self.text(name_node);
            if !name.is_empty() {
                if is_ptr {
                    self.fn_ptr_typedefs.insert(name.to_string());
                } else {
                    self.fn_type_typedefs.insert(name.to_string());
                }
            }
            return;
        }

        // `typedef … declarator, declarator;` — each declarator decides
        // independently (`typedef int X, (*P)(int);` registers only P).
        let mut cursor = node.walk();
        for declarator in node.children_by_field_name("declarator", &mut cursor) {
            if declarator.kind() != "function_declarator" {
                continue;
            }
            // The typedef's NAME binds inside the declarator: unwrap parens —
            // `typedef int (*P)(int)` reaches the `pointer_declarator`; a bare
            // `typedef int F(int)` reaches the `type_identifier` directly.
            let Some(mut inner) = declarator.child_by_field_name("declarator") else {
                continue;
            };
            // `parenthesized_declarator` has no `declarator` field — the inner
            // node is just its first named child.
            while inner.kind() == "parenthesized_declarator" {
                let Some(i) = inner
                    .child_by_field_name("declarator")
                    .or_else(|| inner.named_child(0))
                else {
                    break;
                };
                inner = i;
            }
            let mut name_node = inner;
            while matches!(
                name_node.kind(),
                "pointer_declarator" | "array_declarator" | "reference_declarator"
                    | "parenthesized_declarator"
            ) {
                match name_node
                    .child_by_field_name("declarator")
                    .or_else(|| name_node.named_child(0))
                {
                    Some(i) => name_node = i,
                    None => break,
                }
            }
            if !matches!(name_node.kind(), "type_identifier" | "identifier") {
                continue;
            }
            let name = self.text(name_node);
            if name.is_empty() {
                continue;
            }
            if inner.kind() == "pointer_declarator" {
                self.fn_ptr_typedefs.insert(name.to_string());
            } else {
                self.fn_type_typedefs.insert(name.to_string());
            }
        }
    }

    /// The `field_identifier` a callable member's declarator binds, or None.
    /// `ptr_td`/`fn_td` are the declaration's `type` being a registered
    /// fn-pointer / function-type typedef.
    pub(super) fn callable_field_name(
        &self,
        d: Node<'t>,
        ptr_td: bool,
        fn_td: bool,
    ) -> Option<Node<'t>> {
        stack_guard!();
        match d.kind() {
            // `Ret (*name)(Args)`: a function_declarator whose declarator
            // unwraps through parens to a POINTER declarator bound directly to
            // a field_identifier. A method decl (`Ret name(Args)`), a function
            // returning a fn-ptr (`Ret (*name())(Args)`), a ptr-to-ptr
            // (`Ret (**name)(Args)`), or an array of fn-ptrs
            // (`Ret (*name[N])(Args)`) is not a callable field.
            "function_declarator" => {
                let mut inner = d.child_by_field_name("declarator")?;
                while inner.kind() == "parenthesized_declarator" {
                    inner = inner
                        .child_by_field_name("declarator")
                        .or_else(|| inner.named_child(0))?;
                }
                if inner.kind() != "pointer_declarator" {
                    return None;
                }
                match inner.child_by_field_name("declarator") {
                    Some(name) if name.kind() == "field_identifier" => Some(name),
                    _ => None,
                }
            }
            // `hook_fn read;` — the typedef already carries the `*`.
            "field_identifier" => {
                if ptr_td {
                    Some(d)
                } else {
                    None
                }
            }
            // `cb_t *cbp;` — exactly one star onto a function-type typedef.
            "pointer_declarator" => {
                if fn_td {
                    match d.child_by_field_name("declarator") {
                        Some(inner) if inner.kind() == "field_identifier" => Some(inner),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            // `int (*fp)(int) = &f;` — C++ NSDMI / GNU member initializer.
            "init_declarator" => {
                let inner = d.child_by_field_name("declarator")?;
                self.callable_field_name(inner, ptr_td, fn_td)
            }
            _ => None,
        }
    }

    /// Mint `field` nodes for the callable members of a `field_declaration`.
    /// The node's own span covers the member declaration; the `contains`
    /// edge lands on the enclosing aggregate (create_node's scope stack).
    pub(super) fn extract_callable_fields(&mut self, node: Node<'t>) {
        let type_name = node.child_by_field_name("type").map(|t| self.text(t));
        let ptr_td = type_name.is_some_and(|t| self.fn_ptr_typedefs.contains(t));
        let fn_td = type_name.is_some_and(|t| self.fn_type_typedefs.contains(t));
        let mut cursor = node.walk();
        for declarator in node.children_by_field_name("declarator", &mut cursor) {
            let Some(name_node) = self.callable_field_name(declarator, ptr_td, fn_td) else {
                continue;
            };
            let name = self.text(name_node);
            if name.is_empty() {
                continue;
            }
            self.create_node(
                "field",
                name,
                node,
                Extra {
                    docstring: preceding_docstring(node, self.src),
                    signature: Some(self.text(node).trim().to_string()),
                    ..Extra::default()
                },
            );
        }
    }

    /// recordCppFnPtrBinding (tree-sitter.ts:5089).
    pub(super) fn record_cpp_fn_ptr_binding(&mut self, local_name: &str, value: Option<Node>) {
        let Some(value) = value else { return };
        if value.kind() != "pointer_expression" {
            return;
        }
        if value.child(0).map(|c| c.kind() != "&").unwrap_or(true) {
            return; // `*p` dereference, not address-of
        }
        let arg = value
            .child_by_field_name("argument")
            .or_else(|| value.named_child(0));
        let Some(arg) = arg else { return };
        if !matches!(arg.kind(), "identifier" | "template_function" | "qualified_identifier") {
            return;
        }
        if self.stack.is_empty() {
            return;
        }
        let caller_row = self.top_row();
        let target = strip_cpp_template_args(self.text(arg));
        if target.is_empty() || target == local_name {
            return;
        }
        let targets = self
            .local_fn_ptrs
            .entry(caller_row)
            .or_default()
            .entry(local_name.to_string())
            .or_default();
        if !targets.contains(&target) {
            targets.push(target); // Set semantics, insertion-ordered
        }
    }
}
