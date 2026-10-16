//! Recover member receiver evidence when extraction retained only the method name.
use super::*;
use super::iteration::{descendant_for_position, named_children};

impl KernelResolver {
    /// Plain receivers use lexical AST declarations before any backward text scan.
    pub(super) fn cpp_plain_call(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if r.language != "cpp" || r.reference_kind != "calls" { return Ok(None); }
        // `a->b()->c()`: the class `b` is declared to return, not a namesake of it.
        if r.reference_name.contains("()") { return self.cpp_chain_on_written_type(r); }
        let method = r.reference_name.rsplit(['.', '>']).next().unwrap_or(&r.reference_name);
        if !re!(r"^[A-Za-z_]\w*$").is_match(method) { return Ok(None); }
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&source, r) else { return Ok(None) };
        let mut at = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        while at.kind() != "call_expression" {
            let Some(parent) = at.parent() else { return Ok(None) }; at = parent;
        }
        let Some(function) = at.child_by_field_name("function").filter(|n| n.kind() == "field_expression") else { return Ok(None) };
        let Some(field) = function.child_by_field_name("field") else { return Ok(None) };
        if &source.text()[field.start_byte()..field.end_byte()] != method { return Ok(None); }
        let Some(argument) = function.child_by_field_name("argument") else { return Ok(None) };
        let operator = super::awaited::strip_ts_comments(&source.text()[argument.end_byte()..field.start_byte()]);
        let mut receiver = argument;
        while receiver.kind() == "parenthesized_expression" {
            let children = named_children(receiver);
            let [only] = children.as_slice() else { return Ok(None) }; receiver = *only;
        }
        if receiver.kind() != "identifier" { return Ok(None); }
        let name = &source.text()[receiver.start_byte()..receiver.end_byte()];
        let mut raw = None;
        // Where the declared type is written, which its names are looked up from: the call, or for
        // an `auto` loop's element, where its range's type is declared.
        let mut site = r.clone();
        let mut element = false;
        let mut scope = receiver;
        // The initializer of an `auto` local, which gives it its type.
        let mut initializer = None;
        while let Some(parent) = scope.parent() {
            // `for (auto& [cfh, iter] : pairs)`, `auto [status, iter] = Open();`: the pair or tuple element.
            if let Some(bound) = self.cpp_structured_binding_written(parent, scope, name, &source, r)? {
                let Some(written) = bound else { return Ok(Some(None)) };
                raw = Some(super::cpp_range_for::cpp_plain_type(&written.text));
                site = written.site;
                element = true;
                break;
            }
            if let Some(ty) = cpp_binding_type(parent, scope, name, &source) {
                if super::cpp_range_for::is_cpp_auto(&ty) { initializer = super::cpp_range_for::binding_initializer(parent, scope, name, &source); }
                raw = Some(ty);
                break;
            }
            if parent.kind() == "for_range_loop" && parent.child_by_field_name("declarator").is_some_and(|n| cpp_declarator_name(n, &source) == name) {
                raw = parent.child_by_field_name("type").zip(parent.child_by_field_name("declarator")).map(|(ty, declarator)| cpp_ast_declared_type(ty, declarator, &source));
                // A loop over `auto` elements has the element type of its range.
                if raw.as_deref().is_some_and(super::cpp_range_for::is_cpp_auto) {
                    if let Some(written) = self.cpp_range_for_element(parent, &source, r, 0)? {
                        raw = Some(super::cpp_range_for::cpp_plain_type(&written.text));
                        site = written.site;
                        element = true;
                    }
                }
                break;
            }
            scope = parent;
        }
        if raw.is_none() {
            if let Some(caller) = self.node_by_id(&r.from_node_id)? {
                if let Some((owner_name, _)) = caller.qualified_name.rsplit_once("::") {
                    let visible = self.namespace_visible_files(&r.file_path, "cpp")?;
                    let owners = self.nodes_by_qualified_name(owner_name)?;
                    let owners: Vec<_> = owners.iter().filter(|n| matches!(n.kind.as_str(), "class" | "struct" | "union") && visible.contains(&n.file_path)).collect();
                    if let [owner] = owners.as_slice() { raw = self.cpp_field_type(owner, name, r)?; }
                }
            }
        }
        // A member the class gets from a class it derives from, or a gtest fixture's: where the
        // function lacks the name, C++ looks in its class and the classes it derives from.
        if raw.is_none() {
            if let Some(member) = self.cpp_caller_member(name, r)? {
                raw = Some(member.text);
                site = member.site;
            }
        }
        // An `auto` local has the type its initializer has where that is written: the cast or smart
        // pointer, the class it constructs, or what the function or method called is declared to
        // return. The index keeps only a return type's last name, which a namesake shares.
        if raw.as_deref().is_some_and(super::cpp_range_for::is_cpp_auto) && !element {
            let Some(value) = initializer else { return Ok(None) };
            let Some(written) = self.cpp_initializer_written(value, &source, r)? else { return Ok(None) };
            raw = Some(super::cpp_range_for::cpp_plain_type(&written.text));
            site = written.site;
            element = true;
        }
        let Some(mut raw) = raw else { return Ok(None) };
        if super::cpp_range_for::is_cpp_auto(&raw) { return Ok(None); }
        if raw.contains('[') { return Ok(Some(None)); }
        // `it->data()` on a container's iterator (`std::deque<Slice>::const_iterator it`, an `auto`
        // local holding `logs_.begin()`): a member of the container's element, looked up where the
        // container's type is written. One a class template's own `operator*` writes with its
        // template parameter goes on below.
        if operator.trim() == "->" {
            if let Some(target) = self.cpp_iterator_target(super::cpp_range_for::CppWritten { text: raw.clone(), site: site.clone() }, r)? {
                if super::cpp_range_for::top_level_pointer(&target.text) { return Ok(None); }
                raw = format!("{}*", super::cpp_range_for::cpp_plain_type(&target.text));
                site = target.site;
                element = true;
            }
        }
        let included = self.namespace_visible_files(&r.file_path, "cpp")?;
        if self.cpp_alias_expansion(&raw, &site, 0, &included)?.is_some_and(|alias| alias.pointer || (operator.trim() == "->" && alias.raw.as_deref().is_some_and(|ty| ty.ends_with("::iterator") || ty.ends_with("::const_iterator")))) { return Ok(None); }
        let mut depth = 0usize;
        let mut pointers = 0usize;
        for ch in raw.chars() {
            match ch { '<' => depth += 1, '>' => depth = depth.saturating_sub(1), '*' if depth == 0 => pointers += 1, _ => () }
        }
        let ty = match operator.trim() {
            "." if pointers == 0 => raw.trim_end_matches('&').trim().to_string(),
            "->" if pointers == 1 && !raw.contains('<') => raw.replace(['*', '&'], "").trim().to_string(),
            // A pointer to a class template (`const autovector<MemTable*>*`, a `Table<int, int>`
            // pointer declared over two lines); a library template's members are no project method.
            "->" if pointers == 1 => {
                let pointee = super::cpp_range_for::cpp_pointee(&raw).unwrap_or_default();
                if re!(r"^(?:const\s+)?(?:::)?(?:std|absl)::").is_match(pointee.trim()) { return Ok(Some(None)); }
                super::cpp_range_for::cpp_plain_type(&pointee)
            }
            // A standard smart pointer or optional: `->` reaches a member of the type it holds.
            // So does an `absl::StatusOr`, `absl::optional` or `absl::NoDestructor` of the library's.
            "->" if pointers == 0 => {
                let Some(hit) = re!(r"^(?:const\s+)?(?:::)?((?:std|absl)::[A-Za-z_]\w*)\s*<(.*)>\s*&?$").captures(&raw) else { return Ok(Some(None)) };
                if !CPP_HOLDERS.contains(&&hit[1]) || !self.cpp_library_holder(&hit[1])? { return Ok(Some(None)); }
                let Some(held) = cpp_held_type(&hit[2]) else { return Ok(None) };
                if held.trim_start_matches("::").starts_with("std::") { return Ok(Some(None)); }
                held
            }
            _ => return Ok(Some(None)),
        };
        // A library template's member type: its iterator is the library's, and its `value_type`
        // (`std::vector<Slice>::value_type key`) is its element.
        let mut ty = ty;
        if let Some(member) = cpp_library_member_type(&ty) {
            let Some(held) = member else { return Ok(Some(None)) };
            ty = held;
            element = true;
        }
        let primary = cpp_primary_type(&ty);
        let primary = primary.as_str();
        // A standard type's own member (`iter.reset()` on a `std::unique_ptr` element) is no project method.
        if element && re!(r"^(?:::)?(?:std|absl)::").is_match(primary) { return Ok(Some(None)); }
        if let Some(caller) = self.node_by_id(&site.from_node_id)? { if self.cpp_template_parameter(&caller, primary, &site)? { return Ok(Some(None)); } }
        if self.cpp_alias_expansion(primary, &site, 0, &included)?.is_some_and(|alias| alias.pointer) { return Ok(None); }
        let held = operator.trim() == "->" && pointers == 0;
        // C++ looks a type name up in the classes around the code and their bases before the
        // namespaces: `Iterator` in a class deriving `MemTableRep` is `MemTableRep::Iterator`.
        let mut owner = self.cpp_class_scope_type(primary, &site)?;
        if owner.is_none() { owner = self.cpp_type_owner_visible(primary, &site, 0, false, &included)?; }
        if owner.is_none() && held {
            if let Some(parts) = super::cpp_aliases::type_segments(primary) {
                // A class template and its specializations share a qualified name, and so their members.
                let family = self.cpp_scoped_types(primary, &parts.join("::"), &site, &included)?;
                if let Some(first) = family.first().filter(|f| family.len() > 1 && family.iter().all(|n| n.qualified_name == f.qualified_name && matches!(n.kind.as_str(), "class" | "struct"))) { owner = Some(first.clone()); }
                if owner.is_none() { owner = self.cpp_type_via_using(primary, &parts, &site, &included)?; }
            }
        }
        // A class whose methods alone are indexed: protobuf's `descriptor.pb.h`, too large to index,
        // declares `DescriptorProto`, and `descriptor.pb.cc` defines `DescriptorProto::Clear`.
        if owner.is_none() {
            if let Some(method) = self.cpp_unindexed_class_method(primary, method, &site)? {
                return Ok(Some(Some(KCand { node: method, confidence: 0.85, resolved_by: "instance-method" })));
            }
        }
        let Some(owner) = owner else {
            // A held type that is an alias of a library type holds no project class.
            let aliased_library = held && self.cpp_alias_expansion(primary, &site, 0, &included)?
                .is_some_and(|alias| alias.raw.as_deref().is_some_and(|ty| ty.trim_start_matches("::").starts_with("std::")));
            return Ok(aliased_library.then_some(None));
        };
        // Preserve the existing guarded alias and specialization fallback on a miss. (The owner's
        // qualified name is looked up from the global scope: inside `namespace hpb`, `upb::Arena`
        // would find `hpb::upb::Arena`.)
        let member = self.match_bound_type_member(&format!("::{}", owner.qualified_name), method, r)?;
        // A class that lacks the method, and whose bases lack it too, is called nothing (but a
        // class template's specialization may declare it, and so may a base the index doesn't link,
        // held in a smart pointer or not).
        if member.is_none() && self.supertypes_complete && !self.cpp_alias_template_owner(&owner, r)?
            && (held || self.cpp_methods_named(&owner, method, r)?.is_empty())
            && self.cpp_bases_known(&owner, 0, &mut HashSet::new())? { return Ok(Some(None)); }
        Ok(member.map(Some))
    }

    /// The method `method` of the class `ty` names where only the class's methods are indexed:
    /// looked up as `ty` is, from the scopes around `site` innermost first, and only where no
    /// class, struct, union or alias of that name is indexed at all.
    fn cpp_unindexed_class_method(&mut self, ty: &str, method: &str, site: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let Some(parts) = super::cpp_aliases::type_segments(ty) else { return Ok(None) };
        let global = ty.trim_start().starts_with("::");
        let ty = parts.join("::");
        let scopes: Vec<String> = if global { Vec::new() } else {
            self.node_by_id(&site.from_node_id)?.map(|n| n.qualified_name.split("::").map(str::to_string).collect()).unwrap_or_default()
        };
        let mut names: Vec<String> = (1..scopes.len()).rev().map(|i| format!("{}::{ty}", scopes[..i].join("::"))).collect();
        names.push(ty.clone());
        for owner in names {
            if self.nodes_by_qualified_name(&owner)?.iter().any(|n| matches!(n.language.as_str(), "c" | "cpp")) { return Ok(None); }
            let methods = self.nodes_by_qualified_name(&format!("{owner}::{method}"))?;
            let methods: Vec<_> = methods.iter().filter(|n| n.language == "cpp" && n.kind == "method").cloned().collect();
            match methods.as_slice() {
                [] => continue,
                [only] => return Ok(Some(only.clone())),
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// A class `ty` names from inside a class that declares or inherits it, which
    /// C++ looks up before the namespaces around: the scopes of the caller, innermost
    /// first, each with the classes it derives from.
    pub(super) fn cpp_class_scope_type(&mut self, ty: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        if ty.contains("::") { return Ok(None); }
        let Some(caller) = self.node_by_id(&r.from_node_id)? else { return Ok(None) };
        let scopes: Vec<_> = caller.qualified_name.split("::").map(str::to_string).collect();
        let included = self.namespace_visible_files(&r.file_path, "cpp")?;
        let is_class = |n: &&Arc<KNode>| matches!(n.language.as_str(), "c" | "cpp") && matches!(n.kind.as_str(), "class" | "struct" | "union") && included.contains(&n.file_path);
        for i in (1..=scopes.len()).rev() {
            let enclosing = self.nodes_by_qualified_name(&scopes[..i].join("::"))?;
            let classes: Vec<_> = enclosing.iter().filter(is_class).cloned().collect();
            let [class] = classes.as_slice() else { continue };
            let mut frontier = vec![class.clone()];
            let mut seen = HashSet::new();
            for _ in 0..4 {
                let mut next = Vec::new();
                for c in frontier {
                    if !seen.insert(c.id.clone()) { continue; }
                    let nested = self.nodes_by_qualified_name(&format!("{}::{ty}", c.qualified_name))?;
                    if let [only] = nested.iter().filter(is_class).cloned().collect::<Vec<_>>().as_slice() { return Ok(Some(only.clone())); }
                    if self.supertypes_complete { next.extend(self.supertype_nodes(&c.id)?.into_iter().filter(|n| matches!(n.kind.as_str(), "class" | "struct" | "union"))); }
                }
                if next.is_empty() { break; }
                frontier = next;
            }
        }
        Ok(None)
    }

    pub(super) fn cpp_complex_call(&mut self, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        if r.language != "cpp" || r.reference_kind != "calls" || !re!(r"^[A-Za-z_]\w*$").is_match(&r.reference_name) { return Ok(None); }
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        let Some(tree) = self.parsed_tree(&source, r) else { return Ok(None) };
        let mut node = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
        loop {
            if node.kind() == "call_expression" {
                if let Some(function) = node.child_by_field_name("function").filter(|n| n.kind() == "field_expression") {
                    let field = function.child_by_field_name("field").or_else(|| function.named_child(1));
                    if field.is_some_and(|n| source.text()[n.start_byte()..n.end_byte()] == r.reference_name) {
                        let Some(receiver) = function.child_by_field_name("argument").or_else(|| function.named_child(0)) else { return Ok(Some(None)) };
                        let mut shape = receiver;
                        while shape.kind() == "parenthesized_expression" {
                            let children = named_children(shape);
                            let [only] = children.as_slice() else { return Ok(None) }; shape = *only;
                        }
                        // `(*it)->Visit()`, `(*node).Weight()`; not `(*this)`, which is implicit this's.
                        let dereference = shape.kind() == "pointer_expression" && shape.child_by_field_name("operator").is_some_and(|o| o.kind() == "*")
                            && shape.child_by_field_name("argument").is_some_and(|a| a.kind() != "this");
                        if !dereference && !matches!(shape.kind(), "subscript_expression" | "field_expression") { return Ok(None); }
                        let operator = super::awaited::strip_ts_comments(&source.text()[receiver.end_byte()..field.map_or(receiver.end_byte(), |f| f.start_byte())]);
                        if let Some(found) = self.cpp_unspelled_receiver_call(receiver, operator.trim(), &source, r)? { return Ok(Some(found)); }
                        // A dereference whose type can't be read goes on to the name strategies, as before.
                        if dereference { return Ok(None); }
                        let Some(owner) = self.cpp_expression_owner(receiver, &source, r, 0)? else { return Ok(Some(None)) };
                        let methods = self.nodes_by_qualified_name(&format!("{}::{}", owner.qualified_name, r.reference_name))?;
                        let methods: Vec<_> = methods.iter().filter(|n| n.kind == "method" && n.file_path == owner.file_path).collect();
                        let [method] = methods.as_slice() else { return Ok(Some(None)) };
                        return Ok(Some(Some(KCand { node: (*method).clone(), confidence: 0.9, resolved_by: "instance-method" })));
                    }
                }
                return Ok(None);
            }
            let Some(parent) = node.parent() else { break }; node = parent;
        }
        Ok(None)
    }

    /// A C++ call the extractor records by its bare name on a receiver it drops: `(*it)->Visit()`
    /// over a `std::vector<Node*>`, `(*node).Weight()` on a `Node*`, `it->second.Flush()` through
    /// a map's iterator, `statuses[i].ok()` on an array. It calls a member of the class that
    /// receiver has, read as written (`cpp_call_object`) and looked up as C++ looks it up there;
    /// by its bare name the call went to whichever class had a method of that name.
    ///  - `Some(Some(_))`: the member of that class, or of a class it derives from.
    ///  - `Some(None)`: no project method is called: a library type's (`(*it).size()` over a
    ///    `std::vector<std::string>`), the calling function's template parameter's, or a class that
    ///    surely lacks the method.
    ///  - `None`: the type can't be read or placed.
    fn cpp_unspelled_receiver_call(&mut self, receiver: tree_sitter::Node, operator: &str, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Res<Option<Option<KCand>>> {
        let Some(object) = self.cpp_call_object(receiver, operator, source, r)? else { return Ok(None) };
        if super::cpp_range_for::top_level_pointer(&object.text) { return Ok(None); }
        let plain = super::cpp_range_for::cpp_plain_type(&object.text);
        if let Some(parts) = super::cpp_aliases::type_segments(&plain) {
            let last = parts.last().map(String::as_str).unwrap_or("");
            if parts[0] == "std" || (parts[0] == "absl" && last.starts_with(|c: char| c.is_ascii_lowercase())) { return Ok(Some(None)); }
        }
        let Some(cls) = self.cpp_written_class(&object, r)? else {
            // A template parameter's type names no particular class when the calling function
            // writes it; one another template writes is what the call's specialization makes it.
            if self.cpp_written_template_parameter(&object)? { return Ok((object.site.from_node_id == r.from_node_id).then_some(None)); }
            return Ok(self.cpp_outside_qualified_type(&object.text, &object.site)?.then_some(None));
        };
        if let Some(member) = self.match_bound_type_member(&format!("::{}", cls.qualified_name), &r.reference_name, r)? { return Ok(Some(Some(member))); }
        if self.supertypes_complete && !self.cpp_alias_template_owner(&cls, r)? && self.cpp_methods_named(&cls, &r.reference_name, r)?.is_empty()
            && self.cpp_bases_known(&cls, 0, &mut HashSet::new())? { return Ok(Some(None)); }
        Ok(None)
    }

    fn cpp_expression_owner(&mut self, node: tree_sitter::Node, source: &SourceFile, r: &ResolveRefIn, depth: u32) -> Res<Option<Arc<KNode>>> {
        if depth > 4 { return Ok(None); }
        let text = |n: tree_sitter::Node| &source.text()[n.start_byte()..n.end_byte()];
        match node.kind() {
            "identifier" => {
                // A range variable's type comes from the declared standard container.
                let mut at = node;
                while let Some(parent) = at.parent() {
                    if let Some(raw) = cpp_binding_type(parent, at, text(node), source) {
                        let ty = if raw == "auto" && parent.kind() == "compound_statement" { self.infer_cpp_receiver_type(text(node), r, 0, true)? } else { Some(raw) };
                        let Some(ty) = ty else { return Ok(None) };
                        let primary = cpp_primary_type(&ty);
                        let primary = primary.as_str();
                        if let Some(caller) = self.node_by_id(&r.from_node_id)? { if self.cpp_template_parameter(&caller, primary, r)? { return Ok(None); } }
                        return self.cpp_type_owner(primary, r, 0, false);
                    }
                    if parent.kind() == "for_range_loop" && parent.child_by_field_name("body").is_some_and(|body| body.id() == at.id())
                        && parent.child_by_field_name("declarator").is_some_and(|n| cpp_declarator_name(n, source) == text(node)) {
                        if let Some(right) = parent.child_by_field_name("right").filter(|n| n.kind() == "identifier") {
                            return self.cpp_container_element(text(right), r);
                        }
                        return Ok(None);
                    }
                    at = parent;
                }
                let owner = self.nodes_in_file(&r.file_path)?.iter().filter(|n| matches!(n.kind.as_str(), "class" | "struct" | "union") && (n.start_line, n.start_column) <= (r.line, r.column) && (n.end_line, n.end_column) >= (r.line, r.column)).min_by_key(|n| (n.end_line - n.start_line, n.end_column - n.start_column)).cloned();
                if let Some(owner) = owner { if let Some(field) = self.cpp_field_owner(&owner, text(node), r)? { return Ok(Some(field)); } }
                if let Some(ty) = self.infer_cpp_receiver_type(text(node), r, 0, true)? {
                    if let Some(owner) = self.cpp_type_owner(&ty, r, 0, false)? { return Ok(Some(owner)); }
                }
                Ok(None)
            }
            "subscript_expression" => {
                let Some(argument) = node.child_by_field_name("argument").or_else(|| node.named_child(0)) else { return Ok(None) };
                if argument.kind() == "identifier" && !self.cpp_range_binding(argument, source) {
                    if let Some(element) = self.cpp_container_element(text(argument), r)? { return Ok(Some(element)); }
                }
                let Some(owner) = self.cpp_expression_owner(argument, source, r, depth + 1)? else { return Ok(None) };
                let mut site = r.clone().at(&owner); site.from_node_id = owner.id.clone(); site.column = owner.start_column;
                let mut results = Vec::new();
                for member in self.nodes_by_qualified_name(&format!("{}::operator[]", owner.qualified_name))?.iter().filter(|n| n.kind == "method" && n.file_path == owner.file_path) {
                    let Some(ty) = self.cpp_method_type(member, &site)?.or_else(|| member.return_type.clone()) else { return Ok(None) };
                    if self.cpp_template_parameter(member, &ty, &site)? { return Ok(None); }
                    let included = self.namespace_visible_files(&r.file_path, "cpp")?;
                    let Some(result) = self.cpp_type_owner_visible(&ty, &site, 0, false, &included)? else { return Ok(None) };
                    if !results.iter().any(|n: &Arc<KNode>| n.id == result.id) { results.push(result); }
                }
                Ok(match results.as_slice() { [only] => Some(only.clone()), _ => None })
            }
            "field_expression" => {
                let Some(argument) = node.child_by_field_name("argument").or_else(|| node.named_child(0)) else { return Ok(None) };
                let Some(field) = node.child_by_field_name("field").or_else(|| node.named_child(1)) else { return Ok(None) };
                let Some(owner) = self.cpp_expression_owner(argument, source, r, depth + 1)? else { return Ok(None) };
                self.cpp_field_owner(&owner, text(field), r)

            }
            "parenthesized_expression" => match named_children(node).as_slice() { [only] => self.cpp_expression_owner(*only, source, r, depth + 1), _ => Ok(None) },
            _ => Ok(None),
        }
    }

    fn cpp_method_type(&mut self, method: &KNode, r: &ResolveRefIn) -> Res<Option<String>> {
        let Some(source) = self.read_file(&method.file_path) else { return Ok(None) };
        let mut site = r.clone().at(method); site.from_node_id = method.id.clone(); site.column = method.start_column;
        let Some(tree) = self.parsed_tree(&source, &site) else { return Ok(None) };
        let mut node = descendant_for_position(tree.root_node(), source.text(), ((method.start_line - 1).max(0) as usize, method.start_column.max(0) as usize));
        loop {
            if matches!(node.kind(), "function_definition" | "field_declaration" | "declaration") {
                return Ok(node.child_by_field_name("type").map(|ty| source.text()[ty.start_byte()..ty.end_byte()].to_string()));
            }
            let Some(parent) = node.parent() else { return Ok(None) }; node = parent;
        }
    }

    fn cpp_field_tree(&mut self, source: &Rc<SourceFile>, r: &ResolveRefIn) -> Option<Rc<tree_sitter::Tree>> {
        let normalized = source.cpp_field_tree.get_or_init(|| {
            // Match extraction's padded access-macro rewrite; byte offsets stay fixed.
            let blanked = super::cpp_annotations::cpp_without_annotations(source.text());
            let text = re!(r"(?m)^([ \t]*)([A-Z][A-Z0-9]*_[A-Z0-9_]*)([ \t]*):([ \t]*(?://[^\n]*)?\r?$)").replace_all(&blanked, |m: &regex::Captures| {
                let name = &m[2];
                let keyword = if name.contains("PRIVATE") { "private" } else if name.contains("PROTECTED") { "protected" }
                    else if name.contains("PUBLIC") || matches!(name, "Q_SIGNALS" | "Q_SLOTS") { "public" } else { return m[0].to_string(); };
                let width = name.len() + m[3].len();
                if keyword.len() > width { return m[0].to_string(); }
                format!("{}{}{}:{}", &m[1], keyword, " ".repeat(width - keyword.len()), &m[4])
            });
            if text.as_ref() == blanked.as_ref() { return None; }
            crate::tree::parse_with_cached_parser(text.as_ref(), "cpp").ok().map(Rc::new)
        }).clone();
        normalized.or_else(|| self.parsed_tree(source, r))
    }

    fn cpp_field_owner(&mut self, owner: &Arc<KNode>, field: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let Some(ty) = self.cpp_field_type(owner, field, r)? else { return Ok(None) };
        let primary = cpp_primary_type(&ty);
        let primary = primary.as_str();
        let mut site = r.clone().at(owner); site.from_node_id = owner.id.clone(); site.column = owner.start_column;
        if self.cpp_template_parameter(owner, primary, &site)? { return Ok(None); }
        let included = self.namespace_visible_files(&r.file_path, "cpp")?;
        self.cpp_type_owner_visible(primary, &site, 0, false, &included)
    }

    pub(super) fn cpp_field_type(&mut self, owner: &Arc<KNode>, field: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        Ok(match self.cpp_field_types(owner, field, r)?.as_slice() { [only] => Some(only.clone()), _ => None })
    }

    /// Every type `owner`'s own declaration writes for its data member `field`: one for a member the
    /// class declares, none for a name it doesn't, several for one it declares more than once.
    pub(super) fn cpp_field_types(&mut self, owner: &Arc<KNode>, field: &str, r: &ResolveRefIn) -> Res<Vec<String>> {
        let Some(source) = self.read_file(&owner.file_path) else { return Ok(Vec::new()) };
        let mut site = r.clone().at(owner); site.from_node_id = owner.id.clone(); site.column = owner.start_column;
        let Some(tree) = self.cpp_field_tree(&source, &site) else { return Ok(Vec::new()) };
        let mut node = descendant_for_position(tree.root_node(), source.text(), ((owner.start_line - 1).max(0) as usize, owner.start_column.max(0) as usize));
        while !matches!(node.kind(), "class_specifier" | "struct_specifier" | "union_specifier") {
            let Some(parent) = node.parent() else { break }; node = parent;
        }
        let mut declarations = Vec::new();
        if node.child_by_field_name("name").is_some_and(|name| source.text()[name.start_byte()..name.end_byte()] == owner.name) {
            if let Some(body) = node.child_by_field_name("body") { declarations.extend(named_children(body).into_iter().filter(|n| n.kind() == "field_declaration")); }
        }
        let mut types = Vec::new();
        for declaration in declarations {
            let Some(ty) = declaration.child_by_field_name("type") else { continue };
            let mut cursor = declaration.walk();
            for (index, child) in declaration.children(&mut cursor).enumerate() {
                if declaration.field_name_for_child(index as u32) == Some("declarator") && cpp_declarator_name(child, &source) == field {
                    types.push(cpp_ast_declared_type(ty, child, &source));
                }
            }
        }
        Ok(types)
    }

    fn cpp_range_binding(&self, node: tree_sitter::Node, source: &SourceFile) -> bool {
        let mut at = node;
        while let Some(parent) = at.parent() {
            if parent.kind() == "for_range_loop" && parent.child_by_field_name("body").is_some_and(|body| body.id() == at.id())
                && parent.child_by_field_name("declarator").is_some_and(|n| cpp_declarator_name(n, source) == &source.text()[node.start_byte()..node.end_byte()]) { return true; }
            at = parent;
        }
        false
    }

    fn cpp_container_element(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<Arc<KNode>>> {
        let Some(source) = self.read_file(&r.file_path) else { return Ok(None) };
        if let Some(tree) = self.parsed_tree(&source, r) {
            let mut at = descendant_for_position(tree.root_node(), source.text(), ((r.line - 1).max(0) as usize, r.column.max(0) as usize));
            while let Some(parent) = at.parent() {
                if let Some(raw) = cpp_binding_type(parent, at, name, &source) {
                    let Some(element) = cpp_element_type(&raw) else { return Ok(None) };
                    return self.cpp_type_owner(&element, r, 0, false);
                }
                at = parent;
            }
        }
        let start = self.enclosing_scope_start_line(&r.file_path, "cpp", r.line)?.saturating_sub(1).max(0) as usize;
        for line in source.iter().take(r.line.max(0) as usize).skip(start).rev() {
            if let Some(raw) = self.cpp_declarator_match(line, &regex::escape(name))? {
                let Some(element) = cpp_element_type(&raw) else { return Ok(None) };
                return self.cpp_type_owner(&element, r, 0, false);
            }
        }
        Ok(None)
    }
}

/// Library types whose `->` reaches the value they hold.
pub(super) const CPP_HOLDERS: &[&str] = &["std::unique_ptr", "std::shared_ptr", "std::optional", "absl::optional", "absl::StatusOr", "absl::NoDestructor"];

/// The template arguments of a holder `raw` writes (CPP_HOLDERS): `Iterator` of
/// `std::unique_ptr<Iterator>`, `FeatureResolver` of `absl::StatusOr<FeatureResolver>`.
pub(super) fn cpp_holder_arguments(raw: &str) -> Option<&str> {
    let hit = re!(r"^(?:::)?((?:std|absl)::[A-Za-z_]\w*)\s*<(.*)>$").captures(raw)?;
    CPP_HOLDERS.contains(&hit.get(1)?.as_str()).then(|| hit.get(2).map(|m| m.as_str())).flatten()
}

impl KernelResolver {
    /// Whether the holder `spelled` names is the library's: no type of the project's has its name
    /// (isCppLibraryHolder).
    pub(super) fn cpp_library_holder(&mut self, spelled: &str) -> Res<bool> {
        let last = spelled.rsplit("::").next().unwrap_or(spelled);
        let suffix = format!("::{spelled}");
        Ok(!self.nodes_by_name(last)?.iter().any(|n| matches!(n.language.as_str(), "c" | "cpp")
            && matches!(n.kind.as_str(), "class" | "struct" | "union" | "type_alias") && (n.qualified_name == spelled || n.qualified_name.ends_with(&suffix))))
    }
}

/// The class a subscript of a local reaches: the element of a standard sequence container, or of a
/// one-dimensional array of a class (`Item items[2]`, which the declared type reads as `Item[]`).
fn cpp_element_type(raw: &str) -> Option<String> {
    re!(r"^std::(?:vector|array|deque|span)\s*<\s*([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*[,>]|^([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\[\]$")
        .captures(raw)
        .and_then(|capture| capture.get(1).or_else(|| capture.get(2)))
        .map(|m| m.as_str().to_string())
}

/// The first template argument of a standard smart pointer or optional, or None when it
/// is a pointer: `Foo`, `ns::Foo<int, true>`, `const Foo` for `Foo, Deleter`.
/// The type `ty` names as its owner is looked up: `SkipList` for `SkipList<Key, Comparator>`,
/// and `SkipList::Iterator` for the member type `SkipList<Key, Comparator>::Iterator`, which the
/// class template declares whatever its arguments.
pub(super) fn cpp_primary_type(ty: &str) -> String {
    let mut depth = 0usize;
    let mut member = false;
    for (i, ch) in ty.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => { depth = depth.saturating_sub(1); if depth == 0 && ty[i + 1..].trim_start().starts_with("::") { member = true; } }
            _ => (),
        }
    }
    if member {
        if let Some(parts) = super::cpp_aliases::type_segments(ty) {
            let global = re!(r"^(?:(?:typename|const)\s+)*::").is_match(ty.trim());
            return format!("{}{}", if global { "::" } else { "" }, parts.join("::"));
        }
    }
    ty.split('<').next().unwrap_or(ty).trim().to_string()
}

/// What a library template's member type is: Some(its element) for a container's `value_type`,
/// `reference` or a smart pointer's `element_type` (the first template argument), a map's
/// `mapped_type` (the second); Some(None) for any other member, its `iterator` included, which is
/// the library's, and for a map's `value_type`, a `std::pair` of the key and the value. None for a
/// type that is no library template's member.
fn cpp_library_member_type(ty: &str) -> Option<Option<String>> {
    let hit = re!(r"^(?:(?:typename|const)\s+)*(?:::)?(?:std|absl)::(\w+)\s*<(.*)>\s*::\s*([A-Za-z_]\w*)\s*&?$").captures(ty.trim())?;
    let arguments = &hit[2];
    let map = super::cpp_range_for::LIBRARY_MAPS.contains(&&hit[1]);
    Some(match &hit[3] {
        "value_type" | "reference" | "const_reference" if map => None,
        "value_type" | "reference" | "const_reference" | "element_type" => cpp_held_type(arguments),
        "mapped_type" => {
            let mut depth = 0usize;
            let comma = arguments.char_indices().find(|&(_, ch)| {
                match ch { '<' | '(' => depth += 1, '>' | ')' => depth = depth.saturating_sub(1), _ => () }
                ch == ',' && depth == 0
            });
            comma.and_then(|(at, _)| cpp_held_type(&arguments[at + 1..]))
        }
        _ => None,
    })
}

pub(super) fn cpp_held_type(arguments: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut end = arguments.len();
    for (i, ch) in arguments.char_indices() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => { end = i; break; }
            _ => (),
        }
    }
    let held = arguments[..end].trim();
    let held = held.strip_prefix("const ").unwrap_or(held).trim();
    let mut depth = 0usize;
    for ch in held.chars() {
        match ch { '<' => depth += 1, '>' => depth = depth.saturating_sub(1), '*' if depth == 0 => return None, _ => () }
    }
    (!held.is_empty()).then(|| held.to_string())
}

pub(super) fn cpp_declarator_name<'a>(mut node: tree_sitter::Node, source: &'a SourceFile) -> &'a str {
    while matches!(node.kind(), "reference_declarator" | "pointer_declarator" | "parenthesized_declarator" | "init_declarator" | "array_declarator") {
        let Some(inner) = node.child_by_field_name("declarator").or_else(|| node.named_child(0)) else { return "" }; node = inner;
    }
    if matches!(node.kind(), "identifier" | "field_identifier") { &source.text()[node.start_byte()..node.end_byte()] } else { "" }
}

pub(super) fn cpp_binding_type(parent: tree_sitter::Node, at: tree_sitter::Node, name: &str, source: &SourceFile) -> Option<String> {
    // A `for` or `if` statement's initializer declares a name for the statement, as a block's declaration does.
    let block = matches!(parent.kind(), "compound_statement" | "declaration_list" | "translation_unit");
    let initializer = super::cpp_range_for::statement_initializer(parent);
    if block || initializer.is_some() {
        let children = if block { named_children(parent) } else { initializer.into_iter().collect() };
        for declaration in children.into_iter().rev().filter(|n| n.kind() == "declaration" && n.end_byte() <= at.start_byte()) {
            // C++ parses a parenthesized initializer as a function declarator.
            // Recognize only a standard smart pointer initialized from a known pointer value.
            if let Some(declarator) = declaration.child_by_field_name("declarator").filter(|n| n.kind() == "function_declarator") {
                if declarator.child_by_field_name("declarator").is_some_and(|n| cpp_declarator_name(n, source) == name) {
                    let ty = declaration.child_by_field_name("type")?;
                    let raw = &source.text()[ty.start_byte()..ty.end_byte()];
                    if re!(r"^std::(?:unique_ptr|shared_ptr)\s*<\s*[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*\s*>$").is_match(raw) {
                        let parameters = declarator.child_by_field_name("parameters").map(named_children).unwrap_or_default();
                        if let [parameter] = parameters.as_slice() {
                            if parameter.child_by_field_name("declarator").is_none() {
                                if let Some(argument) = parameter.child_by_field_name("type").filter(|n| n.kind() == "type_identifier") {
                                    let argument = &source.text()[argument.start_byte()..argument.end_byte()];
                                    let mut child = declaration;
                                    while let Some(scope) = child.parent() {
                                        if let Some(bound) = cpp_binding_type(scope, child, argument, source) {
                                            if bound.ends_with('*') && !bound.contains('[') { return Some(raw.to_string()); }
                                            break;
                                        }
                                        child = scope;
                                    }
                                }
                            }
                        }
                    }
                    // This declaration shadows the receiver even when its type is unproven.
                    return Some(String::new());
                }
            }
            let mut cursor = declaration.walk();
            if declaration.children(&mut cursor).enumerate().any(|(i, child)| declaration.field_name_for_child(i as u32) == Some("declarator") && cpp_declarator_binds(child, name, source)) {
                let declarator = named_children(declaration).into_iter().find(|n| cpp_declarator_binds(*n, name, source))?;
                return Some(declaration.child_by_field_name("type").map(|ty| cpp_ast_declared_type(ty, declarator, source)).unwrap_or_default());
            }
        }
    }
    if matches!(parent.kind(), "lambda_expression" | "function_definition") {
        let mut queue = named_children(parent).into_iter().filter(|n| n.kind() != "compound_statement").collect::<Vec<_>>();
        while let Some(parameter) = queue.pop() {
            if parameter.kind() == "parameter_declaration" && parameter.child_by_field_name("declarator").is_some_and(|n| cpp_declarator_binds(n, name, source)) {
                let declarator = parameter.child_by_field_name("declarator")?;
                return Some(parameter.child_by_field_name("type").map(|ty| cpp_ast_declared_type(ty, declarator, source)).unwrap_or_default());
            }
            if parameter.kind() != "compound_statement" { queue.extend(named_children(parameter)); }
        }
    }
    None
}

/// Whether the declaration of the name `node` in scope writes `const` (or `constexpr`) in front of
/// its type: `const Buffer* b`, `constexpr char name[] = "name"`, which the declared type
/// (`cpp_ast_declared_type`) leaves out. A `constexpr` pointer (`constexpr Widget* p`) is itself
/// const, not what it points at.
pub(super) fn cpp_declared_const(node: tree_sitter::Node, source: &SourceFile) -> bool {
    let name = &source.text()[node.byte_range()];
    let leading = |declaration: tree_sitter::Node| declaration.child_by_field_name("type").is_some_and(|ty| {
        let head = &source.text()[declaration.start_byte()..ty.start_byte()];
        re!(r"\bconst\b").is_match(head) || (re!(r"\bconstexpr\b").is_match(head) && !cpp_binds_pointer(declaration, name, source))
    });
    let mut at = node;
    while let Some(parent) = at.parent() {
        let block = matches!(parent.kind(), "compound_statement" | "declaration_list" | "translation_unit");
        let initializer = super::cpp_range_for::statement_initializer(parent);
        if block || initializer.is_some() {
            let children = if block { named_children(parent) } else { initializer.into_iter().collect() };
            for declaration in children.into_iter().rev().filter(|n| n.kind() == "declaration" && n.end_byte() <= at.start_byte()) {
                let mut cursor = declaration.walk();
                if declaration.children(&mut cursor).enumerate().any(|(i, child)| declaration.field_name_for_child(i as u32) == Some("declarator") && cpp_declarator_binds(child, name, source)) {
                    return leading(declaration);
                }
            }
        }
        if parent.kind() == "for_range_loop" && parent.child_by_field_name("body").is_some_and(|body| body.id() == at.id())
            && parent.child_by_field_name("declarator").is_some_and(|d| cpp_declarator_binds(d, name, source)) { return leading(parent); }
        if matches!(parent.kind(), "lambda_expression" | "function_definition") {
            let mut queue = named_children(parent).into_iter().filter(|n| n.kind() != "compound_statement").collect::<Vec<_>>();
            while let Some(parameter) = queue.pop() {
                if parameter.kind() == "parameter_declaration" && parameter.child_by_field_name("declarator").is_some_and(|n| cpp_declarator_binds(n, name, source)) { return leading(parameter); }
                if parameter.kind() != "compound_statement" { queue.extend(named_children(parameter)); }
            }
        }
        at = parent;
    }
    false
}

/// Whether the declarator of `declaration` that binds `name` declares a pointer: `p` in
/// `constexpr Widget* p = &w;`.
fn cpp_binds_pointer(declaration: tree_sitter::Node, name: &str, source: &SourceFile) -> bool {
    let mut cursor = declaration.walk();
    let found = declaration.children(&mut cursor).enumerate()
        .find(|(i, child)| declaration.field_name_for_child(*i as u32) == Some("declarator") && cpp_declarator_binds(*child, name, source))
        .map(|(_, child)| child);
    let Some(mut declarator) = found else { return false };
    if declarator.kind() == "init_declarator" {
        let Some(inner) = declarator.child_by_field_name("declarator") else { return false };
        declarator = inner;
    }
    declarator.kind() == "pointer_declarator"
}

pub(super) fn cpp_ast_declared_type(ty: tree_sitter::Node, mut declarator: tree_sitter::Node, source: &SourceFile) -> String {
    let type_node = if matches!(ty.kind(), "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier") {
        ty.child_by_field_name("name").unwrap_or_else(|| {
            let first = ty.parent().and_then(|p| p.child_by_field_name("declarator")).unwrap_or(declarator);
            let mut name = first;
            while let Some(inner) = name.child_by_field_name("declarator").or_else(|| matches!(name.kind(), "reference_declarator" | "parenthesized_declarator").then(|| name.named_child(0)).flatten()) { name = inner; }
            name
        })
    } else { ty };
    let mut raw = source.text()[type_node.start_byte()..type_node.end_byte()].to_string();
    loop {
        match declarator.kind() {
            "pointer_declarator" => raw.push('*'),
            "reference_declarator" => raw.push('&'),
            "array_declarator" | "structured_binding_declarator" => raw.push_str("[]"),
            _ => (),
        }
        let inner = declarator.child_by_field_name("declarator").or_else(|| (declarator.kind() == "reference_declarator").then(|| declarator.named_child(0)).flatten());
        let Some(inner) = inner else { break }; declarator = inner;
    }
    raw
}

fn cpp_declarator_binds(node: tree_sitter::Node, name: &str, source: &SourceFile) -> bool {
    if cpp_declarator_name(node, source) == name { return true; }
    if matches!(node.kind(), "init_declarator" | "reference_declarator" | "pointer_declarator" | "parenthesized_declarator") {
        return node.child_by_field_name("declarator").or_else(|| node.named_child(0)).is_some_and(|n| cpp_declarator_binds(n, name, source));
    }
    if node.kind() == "structured_binding_declarator" {
        return named_children(node).iter().any(|n| &source.text()[n.start_byte()..n.end_byte()] == name);
    }
    false
}
