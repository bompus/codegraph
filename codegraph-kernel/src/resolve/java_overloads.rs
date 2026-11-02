//! Java overload selection by argument type, and a lambda parameter's type.
use super::*;

/// A parameter of a Java method: its type (`String[]`, generics and package
/// dropped), its type as declared, and whether it is `...`.
#[derive(Clone)]
pub(super) struct JavaParameter {
    pub ty: String,
    pub written: String,
    pub varargs: bool,
}

const JAVA_PRIMITIVES: [&str; 8] = ["boolean", "byte", "char", "short", "int", "long", "float", "double"];
/// `java.lang.String`'s supertypes.
const JAVA_STRING_SUPERTYPES: [&str; 5] = ["CharSequence", "Comparable", "Serializable", "Constable", "ConstantDesc"];
/// Words a declaration pattern can take for a type: `return key;`, `else x`.
const JAVA_STATEMENT_WORDS: [&str; 9] = ["return", "throw", "new", "else", "case", "yield", "assert", "instanceof", "final"];

fn java_primitive(ty: &str) -> bool {
    JAVA_PRIMITIVES.contains(&ty)
}

fn java_boxed(primitive: &str) -> Option<&'static str> {
    Some(match primitive {
        "boolean" => "Boolean",
        "byte" => "Byte",
        "char" => "Character",
        "short" => "Short",
        "int" => "Integer",
        "long" => "Long",
        "float" => "Float",
        "double" => "Double",
        _ => return None,
    })
}

fn java_unboxed(wrapper: &str) -> Option<&'static str> {
    Some(match wrapper {
        "Boolean" => "boolean",
        "Byte" => "byte",
        "Character" => "char",
        "Short" => "short",
        "Integer" => "int",
        "Long" => "long",
        "Float" => "float",
        "Double" => "double",
        _ => return None,
    })
}

fn java_widened(primitive: &str) -> &'static [&'static str] {
    match primitive {
        "byte" => &["short", "int", "long", "float", "double"],
        "short" => &["int", "long", "float", "double"],
        "char" => &["int", "long", "float", "double"],
        "int" => &["long", "float", "double"],
        "long" => &["float", "double"],
        "float" => &["double"],
        _ => &[],
    }
}

/// What `java_argument_type` says of a method reference, and of a lambda with
/// its parameters' count after it: no type of its own until a parameter gives
/// it one.
pub(super) const JAVA_FUNCTION_VALUE: &str = "->";

/// The JDK's functional interfaces: java.util.function's, and `Runnable`,
/// `Callable`, `Comparator`.
fn java_functional_type(ty: &str) -> bool {
    re!(r"^(?:Runnable|Callable|Comparator|(?:Bi|Boolean|Double|Int|Long|Obj(?:Double|Int|Long)|To(?:Double|Int|Long)(?:Bi)?|(?:Double|Int|Long)To(?:Double|Int|Long))?(?:Consumer|Function|Predicate|Supplier)|(?:Double|Int|Long)?(?:Binary|Unary)Operator)$")
        .is_match(ty)
}

/// The JDK's functional interfaces: for each parameter of a lambda written for
/// one, the type argument it has.
fn java_functional_argument(interface: &str) -> Option<&'static [usize]> {
    Some(match interface {
        "Consumer" | "Predicate" | "Function" | "UnaryOperator" => &[0],
        "BinaryOperator" | "Comparator" => &[0, 0],
        "BiConsumer" | "BiFunction" | "BiPredicate" => &[0, 1],
        "ToIntFunction" | "ToLongFunction" | "ToDoubleFunction" => &[0],
        "ObjIntConsumer" | "ObjLongConsumer" | "ObjDoubleConsumer" => &[0, 1],
        _ => return None,
    })
}

fn java_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "interface" | "enum" | "struct" | "trait" | "record" | "annotation" | "type_alias" | "union")
}

impl KernelResolver {
    /// A Java type as an overload compares it: no type arguments or package,
    /// its array markers kept.
    pub(super) fn java_bare_type(written: &str) -> String {
        let mut ty = written.to_string();
        loop {
            let next = re!(r"<[^<>]*>").replace_all(&ty, "").to_string();
            if next == ty {
                break;
            }
            ty = next;
        }
        let ty: String = ty.chars().filter(|c| !c.is_whitespace()).collect();
        let dims_start = ty.trim_end_matches("[]").len();
        let dims = &ty[dims_start..];
        let base = &ty[..dims_start];
        let last = base.rsplit('.').next().unwrap_or(base);
        format!("{last}{dims}")
    }

    /// The parameters a Java method's signature (`Node (String html)`)
    /// declares; `None` when it can't be read.
    pub(super) fn java_parameters(signature: Option<&str>) -> Option<Vec<JavaParameter>> {
        let signature = signature?;
        let open = signature.find('(')?;
        let close = signature.rfind(')')?;
        if close < open {
            return None;
        }
        let list = re!(r"/\*[\s\S]*?\*/")
            .replace_all(&signature[open + 1..close], " ")
            .trim()
            .to_string();
        if list.is_empty() {
            return Some(Vec::new());
        }
        let param_re = re!(r"^([\s\S]+?)(\.\.\.)?\s+[A-Za-z_$][\w$]*((?:\s*\[\s*\])*)$");
        let annotation = re!(r"@[\w.]+(?:\([^()]*\))?");
        let fin = re!(r"\bfinal\b");
        let mut params = Vec::new();
        for written in super::overloads_upstream::split_top_level(&list) {
            let plain = fin.replace_all(&annotation.replace_all(&written, " "), " ").trim().to_string();
            let m = param_re.captures(&plain)?;
            params.push(JavaParameter {
                ty: Self::java_bare_type(&format!("{}{}", &m[1], &m[3])),
                written: m[1].to_string(),
                varargs: m.get(2).is_some(),
            });
        }
        Some(params)
    }

    /// A Java overload's parameters, memoised.
    pub(super) fn java_overload_parameters(
        &mut self,
        n: &KNode,
    ) -> Option<Rc<Vec<JavaParameter>>> {
        if let Some(hit) = self.java_parameters_memo.get(&n.id) {
            return hit.clone();
        }
        let params = if n.language == "java" {
            Self::java_parameters(n.signature.as_deref()).map(Rc::new)
        } else {
            None
        };
        self.java_parameters_memo.insert(n.id.clone(), params.clone());
        params
    }

    /// A Java field, constant or enum constant's type as an overload compares
    /// it.
    fn java_field_type(field: &KNode) -> Option<String> {
        if field.kind == "enum_member" {
            let segments: Vec<&str> = field.qualified_name.split("::").collect();
            return segments.get(segments.len().saturating_sub(2)).map(|s| s.to_string());
        }
        let sig = field.signature.as_deref()?;
        let before = &sig[..sig.rfind(field.name.as_str()).unwrap_or(0)];
        let ty = Self::java_bare_type(before);
        (!ty.is_empty()).then_some(ty)
    }

    /// The declared type of a Java local or parameter in the method around
    /// `r`: the nearest declaration above it. `var` is returned as `"var"`.
    fn java_local_declared_type(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let start = self.enclosing_scope_start_line(&r.file_path, &r.language, r.line)?;
        let declared = Self::cached_regex(&format!(
            r"((?:[A-Za-z_$][\w$]*\.)*[A-Za-z_$][\w$]*(?:\s*<[^;=(){{}}]*>)?(?:\s*\[\s*\])*)(\s*\.\.\.)?\s+{}((?:\s*\[\s*\])*)\s*(?:[=;,):]|$)",
            regex::escape(name)
        ))?;
        let comment = re!(r"^\s*(?://|\*|/\*)");
        let mut i = (r.line.min(lines.len() as i64) - 1).max(0);
        while i >= start - 1 {
            let Some(line) = lines.get(i as usize) else { break };
            if !line.contains(name) || comment.is_match(line) {
                i -= 1;
                continue;
            }
            if let Some(m) = declared.captures(line) {
                if &m[1] == "var" {
                    return Ok(Some("var".to_string()));
                }
                if JAVA_STATEMENT_WORDS.contains(&&m[1]) {
                    i -= 1;
                    continue;
                }
                let varargs = m.get(2).map(|_| "[]").unwrap_or("");
                let ty = Self::java_bare_type(&format!("{}{}{}", &m[1], varargs, &m[3]));
                let base = ty.trim_end_matches("[]").to_string();
                if ty.chars().next().is_some_and(|c| c.is_ascii_uppercase()) || java_primitive(&base) {
                    return Ok(Some(ty));
                }
            }
            i -= 1;
        }
        Ok(None)
    }

    /// The declared type of a field of a class around `r`, its own or one it
    /// inherits, the tightest class first.
    fn java_field_declared_type(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        let mut around: Vec<Arc<KNode>> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|n| java_type_kind(&n.kind) && n.start_line <= r.line && n.end_line >= r.line)
            .cloned()
            .collect();
        around.sort_by_key(|n| std::cmp::Reverse(n.start_line));
        for cls in around {
            if let Some(field) = self.java_static_field(&cls, name)? {
                return Ok(Self::java_field_type(&field));
            }
        }
        Ok(None)
    }

    /// The type a Java name holds: a local's or a parameter's, else a field's.
    /// (A `var` hides the field and says nothing.)
    fn java_named_type(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        match self.java_local_declared_type(name, r)? {
            Some(local) if local == "var" => Ok(None),
            Some(local) => Ok(Some(local)),
            None => self.java_field_declared_type(name, r),
        }
    }

    /// The type a Java receiver names or holds: `Type` of `Type.f()`, or a
    /// value's, `x` or `CONSTANT`.
    fn java_receiver_type(&mut self, name: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        if re!(r"^[A-Z].*[a-z]").is_match(name) {
            return Ok(Some(name.to_string()));
        }
        self.java_named_type(name, r)
    }

    /// The type of the field `x.name` or `Type.name` reads, where `x` has a
    /// project type or is an array.
    fn java_field_access_type(
        &mut self,
        receiver: &str,
        name: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let Some(ty) = self.java_receiver_type(receiver, r)? else { return Ok(None) };
        if ty.ends_with("[]") {
            return Ok((name == "length").then(|| "int".to_string()));
        }
        let Some(written) = Self::java_type_name(&ty) else { return Ok(None) };
        let Some(found) = self.java_type_ref(&written, &r.file_path, r)? else { return Ok(None) };
        let owners: Vec<Arc<KNode>> = match &found.decl {
            Some(decl) => vec![decl.clone()],
            None => {
                let last = found.name.rsplit("::").next().unwrap_or(&found.name).to_string();
                let mut out = Vec::new();
                for d in self.jvm_type_decls(&last)? {
                    if d.language == "java"
                        && (d.qualified_name == found.name
                            || d.qualified_name.ends_with(&format!("::{}", found.name)))
                        && self.is_java_type_nameable(&d, &r.file_path, r)?
                    {
                        out.push(d);
                    }
                }
                out
            }
        };
        let field = match owners.as_slice() {
            [only] => self.java_static_field(only, name)?,
            _ => None,
        };
        Ok(field.as_ref().and_then(|f| Self::java_field_type(f)))
    }

    /// Where the parentheses opened at `open` close in a Java expression; -1
    /// when they don't.
    fn java_closing_paren(text: &str, open: usize) -> Option<usize> {
        let mut depth = 0i64;
        for (i, ch) in text.char_indices().skip_while(|(i, _)| *i < open) {
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
        None
    }

    /// What a Java call written as an argument returns. The receiver is a
    /// local, a parameter, a field or a type, and the method's overloads of
    /// that count must agree. `None` for anything longer.
    fn java_called_type(&mut self, arg: &str, r: &ResolveRefIn) -> Res<Option<String>> {
        let Some(call) = re!(r"^(?:this\s*\.\s*)?([A-Za-z_$][\w$]*)\s*\.\s*([A-Za-z_$][\w$]*)\s*\(")
            .captures(arg)
        else {
            return Ok(None);
        };
        let open = call.get(0).unwrap().end() - 1;
        let Some(close) = Self::java_closing_paren(arg, open) else { return Ok(None) };
        if close != arg.len() - 1 {
            return Ok(None);
        }
        let declared = self.java_receiver_type(&call[1], r)?;
        let Some(written) = declared.as_deref().and_then(Self::java_type_name) else {
            return Ok(None);
        };
        let Some(ty) = self.java_type_ref(&written, &r.file_path, r)? else { return Ok(None) };
        let Some(found) = self.resolve_on_java_type(&ty, &call[2], r)? else { return Ok(None) };
        let method = found.node.clone();
        let list = arg[call.get(0).unwrap().end()..close].trim().to_string();
        let count = if list.is_empty() { 0 } else { super::overloads_upstream::split_top_level(&list).len() };
        let overloads: Vec<Arc<KNode>> = self
            .nodes_in_file(&method.file_path)?
            .iter()
            .filter(|n| {
                n.kind == "method"
                    && n.qualified_name == method.qualified_name
                    && Self::java_parameters(n.signature.as_deref()).map(|p| p.len()) == Some(count)
            })
            .cloned()
            .collect();
        let returned = overloads.first().and_then(|n| n.return_type.clone());
        Ok(returned
            .filter(|ret| overloads.iter().all(|n| n.return_type.as_deref() == Some(ret.as_str())))
            .map(|ret| Self::java_bare_type(&ret)))
    }

    /// Whether `decl` is an anonymous class.
    fn java_anonymous_class(decl: &KNode) -> bool {
        decl.name.contains("$anon@") || decl.name.starts_with('<')
    }

    /// The static type of a Java argument, where its text alone says: a
    /// literal, a cast, a `new`, `this`, a local, a parameter or a field, an
    /// element of one, or a field of one or a call on it. `None` for anything
    /// else (`null` fits every reference type).
    pub(super) fn java_argument_type(
        &mut self,
        arg: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        let arg = arg.trim();
        // A lambda or a method reference: no type of its own until a parameter
        // gives it one.
        if let Some(lambda) = re!(r"^(?:\(([^()]*)\)|[A-Za-z_$][\w$]*)\s*->").captures(arg) {
            let count = match lambda.get(1) {
                None => 1,
                Some(list) if list.as_str().trim().is_empty() => 0,
                Some(list) => super::overloads_upstream::split_top_level(list.as_str()).len(),
            };
            return Ok(Some(format!("{JAVA_FUNCTION_VALUE}{count}")));
        }
        if re!(r"^[\w$.]+(?:<[^()]*>)?(?:\[\])*\s*::\s*[A-Za-z_$][\w$]*$").is_match(arg) {
            return Ok(Some(JAVA_FUNCTION_VALUE.to_string()));
        }
        if re!(r#"^"(?:[^"\\]|\\.)*"(?:\s*\+[\s\S]*)?$"#).is_match(arg) {
            return Ok(Some("String".to_string()));
        }
        if re!(r"^'(?:[^'\\]|\\.)+'$").is_match(arg) {
            return Ok(Some("char".to_string()));
        }
        if arg == "true" || arg == "false" {
            return Ok(Some("boolean".to_string()));
        }
        if re!(r"^-?(?:0[xX][\da-fA-F_]+|0[bB][01_]+|\d[\d_]*)$").is_match(arg) {
            return Ok(Some("int".to_string()));
        }
        if re!(r"^-?(?:0[xX][\da-fA-F_]+|0[bB][01_]+|\d[\d_]*)[lL]$").is_match(arg) {
            return Ok(Some("long".to_string()));
        }
        if re!(r"^-?(?:\d[\d_]*(?:\.[\d_]*)?|\.\d[\d_]*)(?:[eE][-+]?\d+)?[fF]$").is_match(arg) {
            return Ok(Some("float".to_string()));
        }
        if re!(r"^-?(?:(?:\d[\d_]*\.[\d_]*|\.\d[\d_]*)(?:[eE][-+]?\d+)?[dD]?|\d[\d_]*(?:[eE][-+]?\d+[dD]?|[dD]))$").is_match(arg) {
            return Ok(Some("double".to_string()));
        }
        // (Only when nothing follows it: `new Foo().bar()` is what `bar` returns.)
        if let Some(built) = re!(r"^new\s+([\w$.]+)\s*(?:<[^()]*>)?\s*(\(|\[)").captures(arg) {
            if &built[2] == "[" {
                return Ok(Some(format!("{}[]", Self::java_bare_type(&built[1]))));
            }
            let open = built.get(0).unwrap().end() - 1;
            let end = Self::java_closing_paren(arg, open);
            return Ok(match end {
                Some(end) if re!(r"^\s*(?:\{[\s\S]*\})?$").is_match(&arg[end + 1..]) => {
                    Some(Self::java_bare_type(&built[1]))
                }
                _ => None,
            });
        }
        if let Some(cast) = re!(r#"^\(\s*([\w$.]+(?:\s*<[^()]*>)?(?:\s*\[\s*\])*)\s*\)\s*[\w$("']"#)
            .captures(arg)
        {
            return Ok(Some(Self::java_bare_type(&cast[1])));
        }
        if arg == "this" {
            let mut around: Vec<Arc<KNode>> = self
                .nodes_in_file(&r.file_path)?
                .iter()
                .filter(|n| java_type_kind(&n.kind) && n.start_line <= r.line && n.end_line >= r.line)
                .cloned()
                .collect();
            around.sort_by_key(|n| std::cmp::Reverse(n.start_line));
            return Ok(around
                .first()
                .filter(|n| !Self::java_anonymous_class(n))
                .map(|n| n.name.clone()));
        }
        if arg == "null" {
            return Ok(None);
        }
        if let Some(own) = re!(r"^this\s*\.\s*([A-Za-z_$][\w$]*)$").captures(arg) {
            return self.java_field_declared_type(&own[1], r);
        }
        if let Some(element) = re!(r"^([A-Za-z_$][\w$]*)\s*\[[^\[\]]*\]$").captures(arg) {
            let array = self.java_named_type(&element[1], r)?;
            return Ok(array.and_then(|a| a.strip_suffix("[]").map(str::to_string)));
        }
        if let Some(field) = re!(r"^([A-Za-z_$][\w$]*)\s*\.\s*([A-Za-z_$][\w$]*)$").captures(arg) {
            return self.java_field_access_type(&field[1], &field[2], r);
        }
        if re!(r"^[A-Za-z_$][\w$]*$").is_match(arg) {
            return self.java_named_type(arg, r);
        }
        self.java_called_type(arg, r)
    }

    /// The Java classes `decl` inherits, by simple name, up to five levels up.
    fn java_ancestor_names(&mut self, decl: &KNode) -> Res<HashSet<String>> {
        let mut seen: HashSet<String> = HashSet::from([decl.id.clone()]);
        let mut names = HashSet::new();
        let mut frontier: Vec<Arc<KNode>> = vec![Arc::new(decl.clone())];
        for _depth in 0..5 {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for ty in &frontier {
                for sup in self.supertype_nodes(&ty.id)? {
                    if seen.insert(sup.id.clone()) {
                        names.insert(sup.name.clone());
                        next.push(sup);
                    }
                }
            }
            frontier = next;
        }
        Ok(names)
    }

    /// How a Java argument of static type `arg` fits a parameter of type
    /// `param`: 2 the same type, 1.5 by widening (tried before boxing), 1 by
    /// boxing or a supertype, 0 when it can't be told, -1 when the compiler
    /// would refuse it.
    fn java_argument_fit(&mut self, arg: &str, param: &str, r: &ResolveRefIn) -> Res<f64> {
        if arg == param {
            return Ok(2.0);
        }
        let type_variable = re!(r"^[A-Z][A-Z0-9]{0,2}(?:\[\])*$");
        if type_variable.is_match(arg) || type_variable.is_match(param) {
            return Ok(0.0);
        }
        if arg.starts_with(JAVA_FUNCTION_VALUE) {
            return self.java_function_fit(arg, param);
        }
        let project_type = |this: &mut Self, name: &str| -> Res<bool> {
            let base = name.trim_end_matches("[]").to_string();
            Ok(this.jvm_type_decls(&base)?.iter().any(|n| n.language == "java"))
        };
        let project_arg = project_type(self, arg)?;
        let project_param = project_type(self, param)?;
        // A name the project doesn't declare may be a type parameter (`Item`),
        // which takes anything.
        let refused = if param == "String" || project_param { -1.0 } else { 0.0 };
        let value_refused = if java_functional_type(param) { -1.0 } else { refused };
        let arg_primitive = java_primitive(arg);
        let param_primitive = java_primitive(param);
        if arg_primitive && param_primitive {
            return Ok(if java_widened(arg).contains(&param) { 1.5 } else { -1.0 });
        }
        if arg_primitive {
            let boxed = java_boxed(arg).is_some_and(|b| b == param);
            return Ok(if boxed
                || matches!(param, "Object" | "Number" | "Comparable" | "Serializable")
            {
                1.0
            } else {
                value_refused
            });
        }
        if param_primitive {
            let unboxed = java_unboxed(arg);
            return Ok(unboxed
                .filter(|u| *u == param || java_widened(u).contains(&param))
                .map(|_| 1.0)
                .unwrap_or(-1.0));
        }
        if param == "Object" {
            return Ok(1.0);
        }
        if arg.ends_with("[]") != param.ends_with("[]") {
            return Ok(if param == "Serializable" || param == "Cloneable" {
                1.0
            } else if param.ends_with("[]") {
                -1.0
            } else {
                refused
            });
        }
        if arg == "String" {
            return Ok(if JAVA_STRING_SUPERTYPES.contains(&param) { 1.0 } else { value_refused });
        }
        let bare = param.trim_end_matches("[]").to_string();
        if project_arg {
            let decls = self.jvm_type_decls(arg.trim_end_matches("[]"))?;
            for decl in decls.iter().filter(|n| n.language == "java") {
                if self.java_ancestor_names(decl)?.contains(&bare) {
                    return Ok(1.0);
                }
            }
        }
        let _ = r;
        Ok(refused)
    }

    /// How a lambda or a method reference fits a parameter of Java type
    /// `param`: 1 a functional interface of the JDK's, 0 what may be one
    /// (another interface, a library type), -1 what can't take one, or a JDK
    /// interface whose method takes another number of arguments than the
    /// lambda's.
    fn java_function_fit(&mut self, arg: &str, param: &str) -> Res<f64> {
        if java_functional_type(param) {
            let takes = if re!(r"Supplier$|^Runnable$|^Callable$").is_match(param) {
                0
            } else if re!(r"^Bi|BiFunction$|Binary|^Obj|^Comparator$").is_match(param) {
                2
            } else {
                1
            };
            return Ok(if arg == JAVA_FUNCTION_VALUE || arg == format!("{JAVA_FUNCTION_VALUE}{takes}") {
                1.0
            } else {
                -1.0
            });
        }
        if param == "Object" || param == "String" || param.ends_with("[]") || java_primitive(param) {
            return Ok(-1.0);
        }
        let decls = self.jvm_type_decls(param)?;
        let java: Vec<&Arc<KNode>> = decls.iter().filter(|n| n.language == "java").collect();
        Ok(if !java.is_empty() && java.iter().all(|n| n.kind != "interface") {
            -1.0
        } else {
            0.0
        })
    }

    /// Of one class's overloads of a method, those a Java call's arguments
    /// fit: by count, then by what each argument's type says, best first, and
    /// a private one only from its own file. `None` when a signature can't be
    /// read (a Kotlin overload's isn't Java's) or none fits.
    pub(super) fn java_overloads_fitting(
        &mut self,
        overloads: &[Arc<KNode>],
        args: &[String],
        r: &ResolveRefIn,
    ) -> Res<Option<Vec<Arc<KNode>>>> {
        let mut counted: Vec<(Arc<KNode>, Rc<Vec<JavaParameter>>)> = Vec::new();
        for n in overloads {
            let Some(params) = self.java_overload_parameters(n) else {
                return Ok(None);
            };
            if n.visibility.as_deref() == Some("private") && n.file_path != r.file_path {
                continue;
            }
            let fits = match params.last() {
                Some(last) if last.varargs => args.len() + 1 >= params.len(),
                _ => args.len() == params.len(),
            };
            if fits {
                counted.push((n.clone(), params));
            }
        }
        if counted.len() < 2 {
            return Ok(counted.pop().map(|(n, _)| vec![n]));
        }
        let mut types: Vec<Option<String>> = Vec::with_capacity(args.len());
        for arg in args {
            types.push(self.java_argument_type(arg, r)?);
        }
        let mut best = f64::NEG_INFINITY;
        let mut fitting: Vec<Arc<KNode>> = Vec::new();
        for (n, params) in &counted {
            let last = params.last();
            let mut score = 0.0f64;
            for (i, ty) in types.iter().enumerate() {
                let Some(ty) = ty else { continue };
                let param = &params[i.min(params.len() - 1)];
                let whole = i + 1 == params.len() || !param.varargs;
                let fit = if whole {
                    self.java_argument_fit(ty, &param.ty, r)?
                } else {
                    self.java_argument_fit(ty, &format!("{}[]", param.ty), r)?
                        .max(self.java_argument_fit(ty, &param.ty, r)?.min(1.0))
                };
                if fit < 0.0 {
                    score = -1.0;
                    break;
                }
                score += fit;
            }
            if score < 0.0 {
                continue;
            }
            // Java tries a call without variable arity first, if it can make
            // that call: an argument that can't be read says nothing of that.
            let varargs = last.is_some_and(|p| p.varargs);
            let rank = if varargs && types.iter().all(Option::is_some) { score - 0.5 } else { score };
            if rank > best {
                best = rank;
                fitting = vec![n.clone()];
            } else if rank == best {
                fitting.push(n.clone());
            }
        }
        Ok((!fitting.is_empty()).then_some(fitting))
    }

    /// The type of a Java lambda's parameter `name`, read off the method the
    /// lambda is passed to. Only for a call with no receiver, to methods of the
    /// calling file, which must agree on a JDK functional interface with a
    /// type argument of its own.
    pub(super) fn java_lambda_parameter_type(
        &mut self,
        name: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<String>> {
        if !re!(r"^[A-Za-z_$][\w$]*$").is_match(name) {
            return Ok(None);
        }
        let Some(lines) = self.read_file(&r.file_path) else { return Ok(None) };
        let start = (self.enclosing_scope_start_line(&r.file_path, &r.language, r.line)? - 1).max(0);
        let head = Self::cached_regex(&format!(
            r"(?:^|[^\w$.])(?:{}|\(\s*((?:[\w$]+\s*,\s*)*){}(?:\s*,\s*[\w$]+)*\s*\))\s*->",
            regex::escape(name),
            regex::escape(name)
        ))?;
        let comment = re!(r"^\s*(?://|\*|/\*)");
        let mut i = (r.line.min(lines.len() as i64) - 1).max(0);
        while i >= start {
            let Some(line) = lines.get(i as usize) else { break };
            i -= 1;
            if comment.is_match(line) || !line.contains("->") || !line.contains(name) {
                continue;
            }
            let Some(m) = head.captures(line) else { continue };
            // Back from the lambda to the `(` of the call it is an argument of.
            let text = lines[start as usize..=(i + 1) as usize].join("\n");
            let at = text.len() - line.len() + m.get(0).unwrap().end() - 1;
            let bytes = text.as_bytes();
            let mut pos = at as i64;
            let mut depth = 0i64;
            let mut index = 0usize;
            while pos > 0 {
                pos -= 1;
                let c = bytes[pos as usize] as char;
                if c == ')' || c == ']' || c == '}' {
                    depth += 1;
                } else if c == '(' || c == '[' || c == '{' {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                } else if c == ',' && depth == 0 {
                    index += 1;
                }
                if bytes[pos as usize] & 0xC0 == 0x80 {
                    continue;
                }
            }
            if pos < 0 || bytes[pos as usize] as char != '(' {
                return Ok(None);
            }
            let Some(called) = re!(r"(?:^|[^\w$.])(?:this\s*\.\s*)?([A-Za-z_$][\w$]*)\s*(?:<[^<>()]*>)?\s*$")
                .captures(&text[..pos as usize])
            else {
                return Ok(None);
            };
            let position = match m.get(1) {
                Some(preceding) if !preceding.as_str().trim().is_empty() => {
                    preceding.as_str().split(',').count().saturating_sub(1)
                }
                _ => 0,
            };
            let mut found: HashSet<String> = HashSet::new();
            let candidates: Vec<Arc<KNode>> = self
                .nodes_in_file(&r.file_path)?
                .iter()
                .filter(|n| n.kind == "method" && n.name == called[1])
                .cloned()
                .collect();
            for method in candidates {
                let params = Self::java_parameters(method.signature.as_deref());
                let Some(written) = params.as_ref().and_then(|p| p.get(index)).map(|p| p.written.clone())
                else {
                    continue;
                };
                let Some(functional) = re!(r"^(?:[\w$]+\.)*([A-Z][\w$]*)\s*<").captures(&written) else {
                    continue;
                };
                let Some(argument_at) = java_functional_argument(&functional[1]) else { continue };
                let Some(&argument_at) = argument_at.get(position) else { continue };
                let arguments = super::declared_member::angle_arguments(&written);
                let Some(argument) = arguments.get(argument_at) else { continue };
                let argument = re!(r"^\?\s*(?:super|extends)\s+").replace(argument, "").to_string();
                if argument.is_empty() || argument == "?" || re!(r"^[A-Z][A-Z0-9]{0,2}$").is_match(&argument) {
                    return Ok(None);
                }
                found.insert(argument);
            }
            return Ok(if found.len() == 1 { found.into_iter().next() } else { None });
        }
        Ok(None)
    }
}
