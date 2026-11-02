//! Java receiver types read from their declarations: a written package or
//! outer class, a chain of fields, and a library type, which gets no guess.
use super::*;

/// A Java receiver type the project declares.
#[derive(Clone)]
pub(super) struct JavaTypeRef {
    /// The class chain `resolve_method_on_type` matches (`Http::Cookie`).
    pub name: String,
    /// The declaration an import or a written package pins, when one does.
    pub decl: Option<Arc<KNode>>,
}

/// What a Java receiver's declaration says: a project type, or a type the
/// project does not declare (a library class, or a field reached through
/// one), which the caller refuses to answer by name alone.
pub(super) enum JavaDeclared {
    Type(JavaTypeRef),
    External,
}

/// Kinds a Java type declaration can have.
fn java_type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class" | "interface" | "enum" | "struct" | "trait" | "record"
    )
}

impl KernelResolver {
    /// `javaTypeName`: a Java type as written (`Foo`, `java.util.List<Foo>`,
    /// `Map.Entry<K, V>`, `play.api.mvc.BodyParser<B>`, `Foo[]`), type
    /// arguments and array markers dropped. The class chain is joined with
    /// `::` (`Map::Entry`), so a nested type is never taken for a same-named
    /// type nested elsewhere, and a package written in source stays in front
    /// of it, dotted (`play.api.mvc.BodyParser`), for `java_type_ref` to pin
    /// the declaration with. `None` for a primitive.
    pub(super) fn java_type_name(raw: &str) -> Option<String> {
        let mut ty = raw.to_string();
        loop {
            let next = re!(r"<[^<>]*>").replace_all(&ty, "").to_string();
            if next == ty {
                break;
            }
            ty = next;
        }
        let ty = re!(r"\[\s*\]").replace_all(&ty, "").to_string();
        let ty = re!(r"\.\.\.$").replace(&ty, "").to_string();
        let ty: String = ty.chars().filter(|c| !c.is_whitespace()).collect();
        let segments: Vec<&str> = ty.split('.').filter(|s| !s.is_empty()).collect();
        let class_segment = re!(r"^[A-Z][A-Za-z0-9_$]*$");
        let mut first = segments.len();
        while first > 0 && class_segment.is_match(segments[first - 1]) {
            first -= 1;
        }
        let chain = segments[first..].join("::");
        if chain.is_empty() || NON_TYPE_RECEIVER_TOKENS.contains(chain.as_str()) {
            return None;
        }
        let pkg = segments[..first].join(".");
        Some(if pkg.is_empty() {
            chain
        } else {
            format!("{pkg}.{chain}")
        })
    }

    /// The declared type of a Java/Kotlin field node, read from its signature
    /// ("<TypeName> <fieldName>", set by the extractor): generics, a dotted
    /// package and array/varargs markers stripped — for Java, in
    /// `java_type_name`'s form, which keeps a written package and outer
    /// class. `None` for a primitive or a field without a signature.
    pub(super) fn field_signature_type(field: &KNode) -> Option<String> {
        let sig = field.signature.as_deref()?;
        // slice(0, lastIndexOf(name)) — a -1 index drops the last UTF-16 unit.
        let before = match sig.rfind(field.name.as_str()) {
            Some(i) => &sig[..i],
            None => super::names::js_prefix(sig, super::names::utf16_len(sig).saturating_sub(1)),
        };
        let type_raw = before.trim();
        if type_raw.is_empty() {
            return None;
        }
        if field.language == "java" {
            return Self::java_type_name(type_raw);
        }
        let no_generics = re!(r"<[^>]*>").replace_all(type_raw, "");
        let no_array = re!(r"\[\s*\]").replace_all(&no_generics, "").to_string();
        let no_varargs = re!(r"\.\.\.$").replace(&no_array, "");
        let last = no_varargs
            .split(|c: char| c == '.' || c.is_whitespace())
            .rfind(|s| !s.is_empty())?
            .to_string();
        if !last.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            return None;
        }
        Some(last)
    }

    /// Java: the declared type of the field a receiver names — `mHandler`,
    /// `this.mHandler`, or `Outer.this.mHandler`. The field is looked up in
    /// the class enclosing the call and then in each class around it,
    /// tightest first, because an inner or anonymous class reads its outer
    /// classes' fields (`mHandler.post(...)` inside a `new Runnable() {…}`);
    /// `Outer.this.f` names the class to read. A `static final` field is
    /// indexed as a `constant`.
    pub(super) fn java_enclosing_field_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let Some(m) = re!(r"^(?:(?:([A-Za-z0-9_$]+)\.)?this\.)?([A-Za-z0-9_$]+)$")
            .captures(receiver)
        else {
            return Ok(None);
        };
        let outer = m.get(1).map(|g| g.as_str().to_string());
        let field_name = m[2].to_string();
        let in_file = self.nodes_in_file(&r.file_path)?;
        if in_file.is_empty() {
            return Ok(None);
        }
        let mut enclosing: Vec<Arc<KNode>> = in_file
            .iter()
            .filter(|n| {
                matches!(n.kind.as_str(), "class" | "interface" | "enum")
                    && n.language == "java"
                    && n.start_line <= r.line
                    && n.end_line >= r.line
            })
            .cloned()
            .collect();
        enclosing.sort_by_key(|n| std::cmp::Reverse(n.start_line));
        for cls in &enclosing {
            if outer.as_ref().is_some_and(|outer| &cls.name != outer) {
                continue;
            }
            let want = format!("{}::{}", cls.qualified_name, field_name);
            if let Some(field) = in_file.iter().find(|n| {
                matches!(n.kind.as_str(), "field" | "constant")
                    && n.name == field_name
                    && n.qualified_name == want
            }) {
                return Ok(Self::field_signature_type(field));
            }
            if outer.is_some() {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// JVM type declarations named `name` (Java, Kotlin or Scala).
    fn jvm_type_decls(&mut self, name: &str) -> Res<Vec<Arc<KNode>>> {
        Ok(self
            .nodes_by_name(name)?
            .iter()
            .filter(|n| java_type_kind(&n.kind) && same_language_family(&n.language, "java"))
            .cloned()
            .collect())
    }

    /// Whether `decl` is the type one of `fqns` (an import, or a package
    /// written in source) names: the Java declaration of that qualified name
    /// or file, or a Scala / Kotlin one in that package's directory (their
    /// qualified names carry no package, and their files need not be named
    /// after the type).
    fn declares_jvm_fqn(decl: &KNode, fqns: &[String]) -> bool {
        let dotted = decl.qualified_name.replace("::", ".");
        let decl_path = decl.file_path.replace('\\', "/");
        let decl_file = re!(r"\.\w+$").replace(&decl_path, "").to_string();
        let decl_dir = decl_path
            .rfind('/')
            .map(|i| decl_path[..=i].to_string())
            .unwrap_or_default();
        fqns.iter().any(|fqn| {
            let fqn_path = fqn.replace('.', "/");
            if dotted == *fqn
                || decl_file == fqn_path
                || decl_file.ends_with(&format!("/{fqn_path}"))
            {
                return true;
            }
            let pkg_path = fqn_path
                .rfind('/')
                .map(|i| fqn_path[..=i].to_string())
                .unwrap_or_default();
            decl.language != "java"
                && !pkg_path.is_empty()
                && (decl_dir == pkg_path || decl_dir.ends_with(&format!("/{pkg_path}")))
        })
    }

    /// Whether Java file `file_path` can name `decl` by its simple name
    /// without importing it — for a type read from a declaration (a field's,
    /// a local's), where `language_type_visible` judges a name written at a
    /// reference. A Java qualified name reads `package::Outer::Inner`; a
    /// nested type is named that way only in its own file, in its own
    /// package (as `Outer.Inner`), or where its top-level class is imported.
    /// A top-level type of another package is also reachable through a
    /// wildcard import, which the import mappings do not list, so it counts
    /// as visible, and so does a Scala or Kotlin declaration, whose qualified
    /// name carries no package to check. A class also names the nested types
    /// of the classes it extends or implements.
    fn is_java_type_nameable(
        &mut self,
        decl: &KNode,
        file_path: &str,
        r: &ResolveRefIn,
    ) -> Res<bool> {
        if decl.language != "java" || decl.file_path == file_path {
            return Ok(true);
        }
        let segments: Vec<&str> = decl.qualified_name.split("::").collect();
        let has_package = segments.len() > 1
            && segments[0]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase());
        let class_chain = if has_package {
            &segments[1..]
        } else {
            &segments[..]
        };
        if class_chain.len() <= 1 {
            return Ok(true);
        }
        let pkg = if has_package { segments[0] } else { "" };
        let here = self.language_file_scope(file_path, "java")?;
        if pkg == here.package {
            return Ok(true);
        }
        let outer = if pkg.is_empty() {
            class_chain[0].to_string()
        } else {
            format!("{pkg}.{}", class_chain[0])
        };
        if self
            .import_mappings(file_path)?
            .iter()
            .any(|i| i.source == outer || i.source.starts_with(&format!("{outer}.")))
        {
            return Ok(true);
        }
        // `import play.mvc.Http.*;` names Http's nested types.
        let owner_dotted = segments[..segments.len() - 1].join(".");
        if here.demand.contains(&owner_dotted) {
            return Ok(true);
        }
        let ancestors = self.java_file_ancestors(file_path, r)?;
        let owner_qn = segments[..segments.len() - 1].join("::");
        Ok(self
            .nodes_by_qualified_name(&owner_qn)?
            .iter()
            .any(|o| ancestors.contains(&o.id)))
    }

    /// The Java classes and interfaces the classes of `file_path` extend or
    /// implement, up to five levels up.
    fn java_file_ancestors(
        &mut self,
        file_path: &str,
        r: &ResolveRefIn,
    ) -> Res<Rc<HashSet<String>>> {
        if let Some(hit) = self.java_file_ancestors_memo.get(file_path) {
            return Ok(hit.clone());
        }
        let classes: Vec<Arc<KNode>> = self
            .nodes_in_file(file_path)?
            .iter()
            .filter(|n| n.language == "java" && java_type_kind(&n.kind))
            .cloned()
            .collect();
        let mut seen: HashSet<String> = classes.iter().map(|c| c.id.clone()).collect();
        let mut ancestors: HashSet<String> = HashSet::new();
        let mut frontier = classes;
        for _depth in 0..5 {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for cls in &frontier {
                for sup in self.supertype_nodes(&cls.id)? {
                    if sup.language != "java"
                        || !java_type_kind(&sup.kind)
                        || !seen.insert(sup.id.clone())
                    {
                        continue;
                    }
                    ancestors.insert(sup.id.clone());
                    next.push(sup);
                }
            }
            frontier = next;
        }
        let _ = r;
        let ancestors = Rc::new(ancestors);
        self.java_file_ancestors_memo
            .insert(file_path.to_string(), ancestors.clone());
        Ok(ancestors)
    }

    /// What a Java type written in `file_path` (`java_type_name`'s form) is:
    /// a project type, or `None` for a library one (`Parcel`, `Context`,
    /// `List`), which none of the project's methods belongs to. It is a
    /// library type when the project declares no JVM type of its outer name;
    /// when an import, or a package written in source, names a type the
    /// project does not declare (`import java.util.Iterator` beside a project
    /// `UByteArray.Iterator`); when every project type of that name is one
    /// the file cannot name that way (a nested type of another package); or
    /// when a project class declares no nested type of the written name
    /// (`MyLayout.LayoutParams`, inherited from a library class). A short
    /// all-caps name (`T`, `VH`) is taken for a type parameter, whose bound
    /// may be a project type, and left alone.
    fn java_type_ref(
        &mut self,
        written: &str,
        file_path: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<JavaTypeRef>> {
        let chain_start = written.find("::");
        let head = chain_start.map(|i| &written[..i]).unwrap_or(written);
        let last_dot = head.rfind('.');
        let pkg = last_dot.map(|i| &head[..i]).unwrap_or("");
        let chain: Vec<&str> = written[last_dot.map(|i| i + 1).unwrap_or(0)..]
            .split("::")
            .filter(|s| !s.is_empty())
            .collect();
        let Some(outer) = chain.first().copied() else {
            return Ok(None);
        };
        if pkg.is_empty() && chain.len() == 1 && re!(r"^[A-Z][A-Z0-9]{0,2}$").is_match(outer) {
            return Ok(Some(JavaTypeRef {
                name: outer.to_string(),
                decl: None,
            }));
        }
        let decls = self.jvm_type_decls(outer)?;
        if decls.is_empty() {
            return Ok(None);
        }
        let pins: Vec<String> = if !pkg.is_empty() {
            vec![format!("{pkg}.{outer}")]
        } else {
            self.import_mappings(file_path)?
                .iter()
                .filter(|i| i.local_name == outer)
                .map(|i| i.source.clone())
                .collect()
        };
        if pins.is_empty() {
            let mut nameable = false;
            for d in &decls {
                if self.is_java_type_nameable(d, file_path, r)? {
                    nameable = true;
                    break;
                }
            }
            if !nameable {
                return Ok(None);
            }
            if chain.len() == 1 {
                return Ok(Some(JavaTypeRef {
                    name: outer.to_string(),
                    decl: None,
                }));
            }
            let name = chain.join("::");
            let last = chain[chain.len() - 1];
            let nested = self.jvm_type_decls(last)?.iter().any(|n| {
                n.qualified_name == name || n.qualified_name.ends_with(&format!("::{name}"))
            });
            return Ok(nested.then_some(JavaTypeRef { name, decl: None }));
        }
        let mut decl = decls.iter().find(|d| Self::declares_jvm_fqn(d, &pins)).cloned();
        for segment in &chain[1..] {
            let Some(owner) = decl.clone() else { break };
            decl = self
                .jvm_type_decls(segment)?
                .iter()
                .find(|n| n.qualified_name == format!("{}::{}", owner.qualified_name, segment))
                .cloned();
        }
        let Some(decl) = decl else { return Ok(None) };
        let segments: Vec<&str> = decl.qualified_name.split("::").collect();
        let has_package = decl.language == "java"
            && segments.len() > 1
            && segments[0]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase());
        let name = if has_package {
            segments[1..].join("::")
        } else {
            segments.join("::")
        };
        Ok(Some(JavaTypeRef {
            name,
            decl: Some(decl),
        }))
    }

    /// isUndeclaredTypeName (name-matcher.ts): a capitalized name no project
    /// class or interface of the call's language family is named.
    pub(super) fn is_undeclared_type_name(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        Ok(name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && !self
                .nodes_by_name(name)?
                .iter()
                .any(|n| java_type_kind(&n.kind) && same_language_family(&n.language, &r.language)))
    }

    /// The java arm of matchMethodCall's typed-receiver block: a project
    /// type's method, no method at all for a library type or a name no
    /// project class is named, or `None` to let the name strategies answer.
    pub(super) fn java_typed_receiver(
        &mut self,
        written: &str,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<Option<KCand>>> {
        let Some(ty) = self.java_type_ref(written, &r.file_path, r)? else {
            return Ok(Some(None));
        };
        if let Some(hit) = self.resolve_on_java_type(&ty, method, r)? {
            return Ok(Some(Some(hit)));
        }
        let last = written.rsplit(['.', ':']).next().unwrap_or(written).to_string();
        if self.is_undeclared_type_name(&last, r)? {
            return Ok(Some(None));
        }
        Ok(None)
    }

    /// Resolve a method on a Java receiver type: the pinned declaration's own
    /// method first, then `resolve_method_on_type` on the class chain, then —
    /// for a nested chain, which the supertype walk there cannot look up —
    /// the supertypes of its simple name. A Scala or Kotlin declaration pinned
    /// by an import is not followed further than its own methods: the class
    /// chain alone matches a same-named Java type as well.
    pub(super) fn resolve_on_java_type(
        &mut self,
        ty: &JavaTypeRef,
        method: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<KCand>> {
        if let Some(decl) = &ty.decl {
            let own = self
                .nodes_by_name(method)?
                .iter()
                .find(|n| {
                    n.kind == "method"
                        && n.qualified_name == format!("{}::{}", decl.qualified_name, method)
                })
                .cloned();
            if let Some(own) = own {
                return Ok(Some(KCand {
                    node: own,
                    confidence: 0.9,
                    resolved_by: "instance-method",
                }));
            }
            if decl.language != "java" {
                return Ok(None);
            }
        }
        let fqn = ty.decl.as_ref().map(|d| d.qualified_name.replace("::", "."));
        let direct = self.resolve_method_on_type(
            &ty.name,
            method,
            r,
            0.9,
            "instance-method",
            fqn.as_deref(),
        )?;
        if direct.is_some() || !ty.name.contains("::") {
            return Ok(direct);
        }
        let last = ty.name.rsplit("::").next().unwrap_or(&ty.name).to_string();
        for sup in self.supertype_type_nodes(&last, r, false, false)? {
            if let Some(via) = self.resolve_method_on_type(
                &sup.qualified_name,
                method,
                r,
                0.9,
                "instance-method",
                None,
            )? {
                return Ok(Some(via));
            }
        }
        Ok(None)
    }

    /// Java: the declared type of a receiver the field lookup reaches — a
    /// field (`mRepo`, `this.mRepo`, `Outer.this.mRepo`) or a chain of up to
    /// three fields after it (`mState.menu`, `this.mOwner.mRepo`), whose head
    /// may also be a local. Each hop reads the field declared on the previous
    /// hop's type, and reads that field's type with its own file's imports.
    /// `External` as soon as a hop's type is a library one; `None` when a hop
    /// cannot be typed (an inherited field, an untyped local).
    pub(super) fn java_receiver_declared_type(
        &mut self,
        receiver: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<JavaDeclared>> {
        let Some(m) =
            re!(r"^((?:[A-Za-z0-9_$]+\.)?this\.[A-Za-z0-9_$]+|[A-Za-z_$][A-Za-z0-9_$]*)((?:\.[A-Za-z_$][A-Za-z0-9_$]*){0,3})$")
                .captures(receiver)
        else {
            return Ok(None);
        };
        let head = m[1].to_string();
        let segments: Vec<String> = m
            .get(2)
            .map(|g| {
                g.as_str()
                    .trim_start_matches('.')
                    .split('.')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        // A plain receiver's local declaration was already read by the caller.
        let local = if !segments.is_empty() && !head.contains('.') {
            self.infer_local_receiver_type(&head, r, true)?
        } else {
            None
        };
        let head_type = match local {
            Some(t) => Some(t),
            None => self.java_enclosing_field_type(&head, r)?,
        };
        let Some(head_type) = head_type else {
            return Ok(None);
        };
        // A type parameter of the class or method around the call (`TState
        // mTmpState` in `QSTileImpl<TState extends State>`) is no library
        // type: its bound decides, which the caller reads when this answers
        // `None`. As there, only a name no project class has: the bound
        // reader also reads a type argument (`Comparable<ApplicationModule>`)
        // as a parameter.
        if re!(r"^[A-Z][A-Za-z0-9_$]*$").is_match(&head_type)
            && !self
                .nodes_by_name(&head_type)?
                .iter()
                .any(|n| java_type_kind(&n.kind))
            && self.java_type_parameter(&head_type, r)?.is_some()
        {
            return Ok(None);
        }
        let mut written = head_type;
        let mut file_path = r.file_path.clone();
        for segment in &segments {
            let Some(ty) = self.java_type_ref(&written, &file_path, r)? else {
                return Ok(Some(JavaDeclared::External));
            };
            let owner = ty.decl.as_ref().map(|d| d.qualified_name.clone());
            let field = self
                .nodes_by_name(segment)?
                .iter()
                .find(|n| {
                    matches!(n.kind.as_str(), "field" | "constant")
                        && n.language == "java"
                        && match &owner {
                            Some(owner) => {
                                n.qualified_name == format!("{owner}::{segment}")
                            }
                            None => {
                                n.qualified_name.ends_with(&format!("::{}::{segment}", ty.name))
                                    || n.qualified_name == format!("{}::{segment}", ty.name)
                            }
                        }
                })
                .cloned();
            let Some(field) = field else { return Ok(None) };
            let Some(field_type) = Self::field_signature_type(&field) else {
                return Ok(None);
            };
            written = field_type;
            file_path = field.file_path.clone();
        }
        Ok(Some(match self.java_type_ref(&written, &file_path, r)? {
            Some(ty) => JavaDeclared::Type(ty),
            None => JavaDeclared::External,
        }))
    }
}
