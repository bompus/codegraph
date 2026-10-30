//! Python attribute types from class bodies, factories and base classes.
//! Syntax trees distinguish executable assignments/returns from strings,
//! nested definitions and incomplete expressions. All evidence names its
//! source files so incremental sync can reopen dependent edges.
use super::super::bound::{python_def_params, python_param_names, PyFile};
use super::*;
use tree_sitter::Node as PyNode;

#[cfg_attr(feature = "addon", napi(object))]
pub struct PythonAttributeResult {
    /// True for a proven type (including an outside/builtin type with no edge).
    /// False leaves the existing name strategies in charge.
    pub claimed: bool,
    pub target_node_id: Option<String>,
    pub type_from: Vec<String>,
}

#[derive(Clone, Debug)]
enum Answer {
    Class(Arc<KNode>),
    Declared(Arc<KNode>),
    Runtime,
    Unknown,
    Absent,
}
type FactoryKey = (String, Option<String>, bool);
#[derive(Default)]
pub(super) struct PythonTypesMemo {
    attributes: HashMap<(String, String), (Answer, Vec<String>)>,
    factories: HashMap<FactoryKey, (Answer, Vec<String>)>,
}
fn text<'a>(node: PyNode, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}
fn children(node: PyNode<'_>) -> Vec<PyNode<'_>> {
    let mut c = node.walk();
    node.named_children(&mut c).collect()
}
fn named_definition<'t>(
    root: PyNode<'t>,
    kind: &str,
    name: &str,
    line: i64,
    source: &str,
) -> Option<PyNode<'t>> {
    let mut pending = vec![root];
    while let Some(n) = pending.pop() {
        if n.kind() == kind
            && (n.start_position().row as i64) < line
            && n.end_position().row as i64 + 1 >= line
            && n.child_by_field_name("name")
                .is_some_and(|v| text(v, source) == name)
        {
            return Some(n);
        }
        pending.extend(children(n));
    }
    None
}
fn agree(answers: Vec<Answer>) -> Answer {
    let mut selected = Answer::Absent;
    for answer in answers {
        match (&selected, &answer) {
            (_, Answer::Absent) => {}
            (Answer::Absent, _) => selected = answer,
            (Answer::Runtime, Answer::Runtime) => {}
            (Answer::Class(a) | Answer::Declared(a), Answer::Class(b) | Answer::Declared(b)) if a.id == b.id => {}
            _ => return Answer::Unknown,
        }
    }
    selected
}
fn simple_name(value: &str) -> bool {
    re!(r"^[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*$").is_match(value)
}
fn annotation_name(raw: &str) -> Option<&str> {
    let mut value = raw.trim().trim_matches(['\'', '"']);
    if let Some(inner) = value
        .strip_prefix("Optional[")
        .or_else(|| value.strip_prefix("typing.Optional["))
    {
        value = inner.strip_suffix(']')?.trim();
    }
    if let Some(inner) = value.strip_suffix("| None") {
        value = inner.trim();
    }
    simple_name(value).then_some(value)
}

impl KernelResolver {
    fn python_named_answer(
        &mut self,
        raw: &str,
        site: &ResolveRefIn,
        read: &mut HashSet<String>,
    ) -> Res<Answer> {
        let Some(name) = annotation_name(raw) else {
            return Ok(Answer::Unknown);
        };
        if matches!(name, "Any" | "object" | "typing.Any") {
            return Ok(Answer::Unknown);
        }
        let root = name.split('.').next().unwrap_or(name);
        let binding = self.receiver_binding(root, site)?;
        if binding.as_ref().is_some_and(|b| matches!(b.kind.as_str(), "local" | "param")) {
            return Ok(Answer::Unknown);
        }
        if let Some(class) = self.python_ref_class(name, site)? {
            read.insert(class.file_path.clone());
            return Ok(Answer::Class(class));
        }
        if matches!(
            name,
            "list" | "dict" | "str" | "set" | "tuple" | "int" | "float" | "bytes" | "bool"
        ) {
            return Ok(Answer::Runtime);
        }
        let imports = self.import_mappings_from_bindings(&binding.into_iter().filter(|b| b.kind == "import").collect::<Vec<_>>());
        for import in imports.iter().filter(|m| m.local_name == root) {
            if self
                .python_module_path(&import.source, &site.file_path)?
                .is_none()
            {
                return Ok(Answer::Runtime);
            }
        }
        Ok(Answer::Unknown)
    }
    fn python_value_answer(
        &mut self,
        value: PyNode,
        source: &str,
        site: &ResolveRefIn,
        class_receiver: Option<&Arc<KNode>>,
        read: &mut HashSet<String>,
        depth: usize,
    ) -> Res<Answer> {
        if depth >= 8 || value.has_error() {
            return Ok(Answer::Unknown);
        }
        match value.kind() {
            "none" => Ok(Answer::Absent),
            "list"
            | "dictionary"
            | "set"
            | "tuple"
            | "string"
            | "integer"
            | "float"
            | "true"
            | "false"
            | "list_comprehension"
            | "dictionary_comprehension"
            | "set_comprehension" => Ok(Answer::Runtime),
            "await" | "parenthesized_expression" => match value.named_child(0) {
                Some(v) => {
                    self.python_value_answer(v, source, site, class_receiver, read, depth + 1)
                }
                None => Ok(Answer::Unknown),
            },
            "conditional_expression" => {
                let items = children(value);
                if items.len() != 3 {
                    return Ok(Answer::Unknown);
                }
                let a = self.python_value_answer(
                    items[0],
                    source,
                    site,
                    class_receiver,
                    read,
                    depth + 1,
                )?;
                let b = self.python_value_answer(
                    items[2],
                    source,
                    site,
                    class_receiver,
                    read,
                    depth + 1,
                )?;
                Ok(agree(vec![a, b]))
            }
            "identifier" => {
                let name = text(value, source);
                let mut scope = value.parent();
                while let Some(n) = scope {
                    if n.kind() == "function_definition" {
                        let mut pending = n
                            .child_by_field_name("body")
                            .into_iter()
                            .collect::<Vec<_>>();
                        while let Some(node) = pending.pop() {
                            if node.start_byte() >= value.start_byte()
                                || matches!(
                                    node.kind(),
                                    "function_definition" | "class_definition" | "lambda"
                                )
                            {
                                continue;
                            }
                            if matches!(
                                node.kind(),
                                "assignment" | "augmented_assignment" | "named_expression"
                            ) && node
                                .child_by_field_name("left")
                                .or_else(|| node.child_by_field_name("name"))
                                .is_some_and(|left| text(left, source) == name)
                            {
                                return Ok(Answer::Unknown);
                            }
                            pending.extend(children(node));
                        }
                        if let Some(params) = n.child_by_field_name("parameters") {
                            for param in children(params) {
                                if matches!(
                                    param.kind(),
                                    "typed_parameter" | "typed_default_parameter"
                                ) && param
                                    .child_by_field_name("name")
                                    .or_else(|| param.named_child(0))
                                    .is_some_and(|target| text(target, source) == name)
                                {
                                    if let Some(ty) = param.child_by_field_name("type") {
                                        return self.python_named_answer(
                                            text(ty, source),
                                            site,
                                            read,
                                        ).map(|answer| match answer { Answer::Class(class) => Answer::Declared(class), other => other });
                                    }
                                }
                            }
                        }
                        break;
                    }
                    scope = n.parent();
                }
                Ok(Answer::Unknown)
            }
            "call" => {
                let Some(function) = value.child_by_field_name("function") else {
                    return Ok(Answer::Unknown);
                };
                let name = text(function, source).trim();
                if !simple_name(name) {
                    return Ok(Answer::Unknown);
                }
                if name == "cls" {
                    return Ok(class_receiver
                        .cloned()
                        .map_or(Answer::Unknown, Answer::Class));
                }
                self.python_callable_result(
                    name,
                    site,
                    read,
                    depth,
                    value.parent().is_some_and(|p| p.kind() == "await"),
                )
            }
            _ => Ok(Answer::Unknown),
        }
    }
    fn python_callable_shadowed(&mut self, name: &str, site: &ResolveRefIn) -> Res<bool> {
        if self.receiver_binding(name, site)?.is_some_and(|b| matches!(b.kind.as_str(), "local" | "param")) {
            return Ok(true);
        }
        let Some(file) = self.read_file(&site.file_path) else { return Ok(false) };
        let Some(tree) = self.parsed_tree(&file, site) else { return Ok(false) };
        let source = file.text();
        let mut frames = vec![tree.root_node()];
        let mut function = None;
        while let Some(node) = frames.pop() {
            if node.start_position().row as i64 + 1 > site.line || (node.end_position().row as i64) < site.line - 1 { continue; }
            if node.kind() == "function_definition" { function = Some(node); }
            frames.extend(children(node));
        }
        let Some(body) = function.and_then(|f| f.child_by_field_name("body")) else { return Ok(false) };
        let mut pending = vec![body];
        while let Some(node) = pending.pop() {
            if matches!(node.kind(), "function_definition" | "class_definition" | "lambda") { continue; }
            if node.kind() == "named_expression" && node.child_by_field_name("name")
                .is_some_and(|target| text(target, source) == name) { return Ok(true); }
            pending.extend(children(node));
        }
        Ok(false)
    }
    fn python_callable_result(
        &mut self,
        name: &str,
        site: &ResolveRefIn,
        read: &mut HashSet<String>,
        depth: usize,
        awaited: bool,
    ) -> Res<Answer> {
        // A parameter/local named after a class or factory cancels
        // module-scope evidence, including assignments later in a def.
        let root = name.split('.').next().unwrap_or(name);
        if self.python_callable_shadowed(root, site)? {
            return Ok(Answer::Unknown);
        }
        let named = self.python_named_answer(name, site, read)?;
        if matches!(named, Answer::Class(_) | Answer::Runtime) {
            return Ok(named);
        }
        let (factory, receiver) = if let Some((owner, method)) = name.rsplit_once('.') {
            match self.python_named_answer(owner, site, read)? {
                Answer::Class(class) => {
                    let members = self.python_members(&class, method, site, &mut HashSet::new())?;
                    (
                        if members.len() == 1 {
                            members.first().cloned()
                        } else {
                            None
                        },
                        Some(class),
                    )
                }
                _ => (self.python_factory_function(name, site)?, None),
            }
        } else {
            (self.python_factory_function(name, site)?, None)
        };
        let Some(factory) = factory else {
            return Ok(Answer::Unknown);
        };
        self.python_factory_answer(&factory, receiver.as_ref(), site, read, depth + 1, awaited)
    }
    fn python_factory_function(
        &mut self,
        name: &str,
        site: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        if let Some(hit) = self.resolve_via_import(&site.clone().naming(name, "references"))? {
            if hit.node.kind == "function" {
                return Ok(Some(hit.node));
            }
        }
        let candidates: Vec<_> = self
            .nodes_in_file(&site.file_path)?
            .iter()
            .filter(|n| n.kind == "function" && n.name == name && !n.qualified_name.contains("::"))
            .cloned()
            .collect();
        Ok(if candidates.len() == 1 {
            candidates.first().cloned()
        } else {
            None
        })
    }
    fn python_factory_answer(
        &mut self,
        factory: &Arc<KNode>,
        receiver: Option<&Arc<KNode>>,
        site: &ResolveRefIn,
        read: &mut HashSet<String>,
        depth: usize,
        awaited: bool,
    ) -> Res<Answer> {
        if depth >= 8 {
            return Ok(Answer::Unknown);
        }
        let key = (factory.id.clone(), receiver.map(|c| c.id.clone()), awaited);
        if let Some((answer, files)) = self.py_globals.types.factories.get(&key) {
            read.extend(files.iter().cloned());
            return Ok(answer.clone());
        }
        let mut own = HashSet::new();
        let answer =
            self.python_factory_answer_uncached(factory, receiver, site, &mut own, depth, awaited)?;
        read.extend(own.iter().cloned());
        if self.py_globals.types.factories.len() >= 8192 {
            self.py_globals.types.factories.clear();
        }
        self.py_globals
            .types
            .factories
            .insert(key, (answer.clone(), own.into_iter().collect()));
        Ok(answer)
    }
    fn python_factory_answer_uncached(
        &mut self,
        factory: &Arc<KNode>,
        receiver: Option<&Arc<KNode>>,
        site: &ResolveRefIn,
        read: &mut HashSet<String>,
        depth: usize,
        awaited: bool,
    ) -> Res<Answer> {
        read.insert(factory.file_path.clone());
        let fs = site.clone().at(factory);
        let Some(file) = self.read_file(&factory.file_path) else {
            return Ok(Answer::Unknown);
        };
        let Some(tree) = self.parsed_tree(&file, &fs) else {
            return Ok(Answer::Unknown);
        };
        let source = file.text();
        let Some(def) = named_definition(
            tree.root_node(),
            "function_definition",
            &factory.name,
            factory.start_line,
            source,
        ) else {
            return Ok(Answer::Unknown);
        };
        if !awaited && text(def, source).trim_start().starts_with("async ") {
            return Ok(Answer::Runtime);
        }
        if let Some(ty) = def.child_by_field_name("return_type") {
            let name = text(ty, source).trim().trim_matches(['\'', '"']);
            if name == "Self" || name == "typing.Self" {
                return Ok(receiver.cloned().map_or(Answer::Unknown, Answer::Class));
            }
            return self.python_named_answer(name, &fs, read);
        }
        let mut stack = def
            .child_by_field_name("body")
            .into_iter()
            .collect::<Vec<_>>();
        let mut answers = Vec::new();
        while let Some(node) = stack.pop() {
            if matches!(
                node.kind(),
                "function_definition" | "class_definition" | "lambda"
            ) {
                continue;
            }
            if node.kind() == "yield" {
                return Ok(Answer::Runtime);
            }
            if node.kind() == "return_statement" {
                if let Some(value) = node.named_child(0) {
                    let mut at = fs.clone();
                    at.line = node.start_position().row as i64 + 1;
                    answers.push(self.python_value_answer(
                        value,
                        source,
                        &at,
                        receiver,
                        read,
                        depth + 1,
                    )?);
                }
            } else {
                stack.extend(children(node));
            }
        }
        Ok(agree(answers))
    }
    fn python_attribute_type(
        &mut self,
        class: &Arc<KNode>,
        attr: &str,
        site: &ResolveRefIn,
        read: &mut HashSet<String>,
        seen: &mut HashSet<String>,
    ) -> Res<Answer> {
        if seen.len() >= 8 || !seen.insert(class.id.clone()) {
            return Ok(Answer::Absent);
        }
        let key = (class.id.clone(), attr.to_string());
        if let Some((answer, files)) = self.py_globals.types.attributes.get(&key) {
            read.extend(files.iter().cloned());
            return Ok(answer.clone());
        }
        let mut own = HashSet::new();
        let answer = self.python_attribute_type_uncached(class, attr, site, &mut own, seen)?;
        read.extend(own.iter().cloned());
        if self.py_globals.types.attributes.len() >= 8192 {
            self.py_globals.types.attributes.clear();
        }
        self.py_globals
            .types
            .attributes
            .insert(key, (answer.clone(), own.into_iter().collect()));
        Ok(answer)
    }
    fn python_attribute_type_uncached(
        &mut self,
        class: &Arc<KNode>,
        attr: &str,
        site: &ResolveRefIn,
        read: &mut HashSet<String>,
        seen: &mut HashSet<String>,
    ) -> Res<Answer> {
        read.insert(class.file_path.clone());
        let fs = site.clone().at(class);
        let Some(file) = self.read_file(&class.file_path) else {
            return Ok(Answer::Unknown);
        };
        let Some(tree) = self.parsed_tree(&file, &fs) else {
            return Ok(Answer::Unknown);
        };
        let source = file.text();
        let Some(def) = named_definition(
            tree.root_node(),
            "class_definition",
            &class.name,
            class.start_line,
            source,
        ) else {
            return Ok(Answer::Unknown);
        };
        let Some(body) = def.child_by_field_name("body") else {
            return Ok(Answer::Unknown);
        };
        let mut stack: Vec<_> = children(body).into_iter().map(|n| (n, 0usize)).collect();
        let mut mentioned = false;
        let mut constructor_only = true;
        let mut answers = Vec::new();
        while let Some((node, functions)) = stack.pop() {
            if node.kind() == "class_definition" || node.kind() == "lambda" {
                continue;
            }
            let functions = functions + usize::from(node.kind() == "function_definition");
            if node.kind() == "assignment" {
                if let Some(left) = node.child_by_field_name("left") {
                    let target = text(left, source).trim();
                    let matches = if functions == 0 {
                        target == attr
                            || target == format!("self.{attr}")
                            || target == format!("cls.{attr}")
                    } else {
                        target == format!("self.{attr}") || target == format!("cls.{attr}")
                    };
                    if matches {
                        mentioned = true;
                        let mut scope = node.parent();
                        while let Some(parent) = scope {
                            if parent.kind() == "function_definition" {
                                constructor_only &= functions == 1 && parent.child_by_field_name("name")
                                    .is_some_and(|name| text(name, source) == "__init__");
                                break;
                            }
                            if parent == def { break; }
                            scope = parent.parent();
                        }
                        if functions > 1 {
                            answers.push(Answer::Unknown);
                            continue;
                        }
                        let mut at = fs.clone();
                        at.line = node.start_position().row as i64 + 1;
                        if let Some(ty) = node.child_by_field_name("type") {
                            answers.push(match self.python_named_answer(text(ty, source), &at, read)? {
                                Answer::Class(class) => Answer::Declared(class), other => other,
                            });
                        } else if let Some(value) = node.child_by_field_name("right") {
                            let mut answer =
                                self.python_value_answer(value, source, &at, None, read, 0)?;
                            // Class-body outside calls may be descriptors whose
                            // instance value differs from the declared object.
                            if functions == 0
                                && value.kind() == "call"
                                && matches!(answer, Answer::Runtime)
                            {
                                answer = Answer::Unknown;
                            }
                            answers.push(answer);
                        }
                    }
                }
            }
            stack.extend(children(node).into_iter().map(|n| (n, functions)));
        }
        let concrete = answers.iter().any(|a| matches!(a, Answer::Class(_) | Answer::Declared(_) | Answer::Runtime));
        let own = mentioned.then(|| {
            let answer = if answers.is_empty() { Answer::Unknown } else { agree(answers) };
            // Conflicting assignments must not fall through to name guessing.
            if concrete && matches!(answer, Answer::Unknown) { Answer::Runtime } else { answer }
        });
        if let Some(answer) = &own {
            // An opaque assignment shadows inherited evidence. Constructor
            // assignments establish the subclass's field after base init.
            if matches!(answer, Answer::Unknown) || constructor_only {
                return Ok(answer.clone());
            }
        }
        let mut answers = Vec::new();
        if let Some(bases) = def.child_by_field_name("superclasses") {
            for base in children(bases) {
                let mut name = text(base, source).trim();
                if let Some((head, _)) = name.split_once('[') {
                    name = head;
                }
                match self.python_named_answer(name,&fs,read)? {
                    Answer::Class(base)=>answers.push(self.python_attribute_type(&base,attr,site,read,seen)?),
                    Answer::Runtime if !re!(r"^(?:object|ABC|Protocol|NamedTuple|TypedDict|Enum|IntEnum|StrEnum|Flag|IntFlag|\w*(?:Error|Exception|Warning))$").is_match(name.rsplit('.').next().unwrap_or(name))=>answers.push(Answer::Runtime),
                    _=>{},
                }
            }
        }
        let inherited = agree(answers);
        if let Some(own) = own {
            if let (Answer::Class(class) | Answer::Declared(class), Answer::Declared(base)) = (&own, &inherited) {
                if self.python_derives_from(class, base, site, &mut HashSet::new())? { return Ok(own); }
            }
            let combined = agree(vec![own, inherited]);
            return Ok(if matches!(combined, Answer::Unknown) { Answer::Runtime } else { combined });
        }
        Ok(inherited)
    }
    fn python_attribute_verdict(&mut self, r: &ResolveRefIn) -> Res<PythonAttributeResult> {
        let mut result = PythonAttributeResult {
            claimed: false,
            target_node_id: None,
            type_from: Vec::new(),
        };
        if r.language != "python" || r.reference_kind != "calls" {
            return Ok(result);
        }
        let Some((receiver, method)) = r.reference_name.rsplit_once('.') else {
            return Ok(result);
        };
        let mut read = HashSet::new();
        let mut strict_local = false;
        let answer = if let Some(callee) = receiver.strip_suffix("()") {
            if !simple_name(callee) {
                return Ok(result);
            }
            let root = callee.split('.').next().unwrap_or(callee);
            if self.python_callable_shadowed(root, r)? {
                result.claimed = true;
                return Ok(result);
            }
            self.python_callable_result(callee, r, &mut read, 0, false)?
        } else if let Some((head, attr)) = receiver
            .split_once('.')
            .filter(|(_, a)| simple_name(a) && !a.contains('.'))
        {
            if self.python_outside_module_receiver(head, r)? {
                result.claimed = true;
                return Ok(result);
            }
            if self
                .import_mappings(&r.file_path)?
                .iter()
                .any(|m| m.local_name == head && m.is_namespace)
                && !self.python_binds_locally(head, &r.file_path, r.line, false)?
            {
                result.claimed = true;
                return Ok(result);
            }
            let class = if head == "self" || head == "cls" {
                self.nodes_in_file(&r.file_path)?
                    .iter()
                    .filter(|n| n.kind == "class" && n.start_line <= r.line && n.end_line >= r.line)
                    .max_by_key(|n| n.start_line)
                    .cloned()
            } else if head.starts_with(char::is_uppercase) {
                self.python_ref_class(head, r)?
            } else {
                match self.infer_local_receiver_type(head, r, true)? {
                    Some(ty) => self.python_ref_class(&ty, r)?,
                    None => None,
                }
            };
            let Some(class) = class else {
                return Ok(result);
            };
            self.python_attribute_type(&class, attr, r, &mut read, &mut HashSet::new())?
        } else {
            // matchPythonTypedLocalCall (#2583): `reg.get()`-shaped calls —
            // a plain local or parameter receiver on a builtin-named method —
            // settle only through the strict scope evidence read; a name it
            // cannot type, or whose evidence contests, stays refused rather
            // than name-guessed. A `self`/`cls` receiver or a builtin type
            // name keeps the ordinary paths.
            if !re!(r"^[A-Za-z_]\w*$").is_match(receiver)
                || !PYTHON_BUILT_IN_METHODS.contains(method)
                || PYTHON_BUILT_IN_TYPES.contains(receiver)
                || receiver == "self"
                || receiver == "cls"
            {
                return Ok(result);
            }
            strict_local = true;
            // A receiver that names a class declared here is a static-style
            // call (`File.write(f, "x")`), which the ordinary paths resolve;
            // the strict read is for a local or a parameter.
            if self.python_ref_class(receiver, r)?.is_some() {
                return Ok(result);
            }
            // A lambda's own parameter of the name shadows the outer binding;
            // its default is that outer value, which the ordinary paths read.
            if self.python_lambda_parameter_named(receiver, r)? {
                return Ok(result);
            }
            // A binding a branch, loop or `try` may not run leaves no single
            // value in effect; claim it and link nothing.
            if self.python_conditional_bindings(receiver, r)? {
                result.claimed = true;
                return Ok(result);
            }
            // A lambda body reads its captured cell when invoked, and a name
            // no binding reaches it yet (a future initialization) stays
            // unresolved there, as the fork's receiver-scope rules hold.
            let future = self.receiver_binding(receiver, r)?.is_some_and(|b| b.line > r.line);
            if future && self.python_in_lambda_body(r)? {
                result.claimed = true;
                return Ok(result);
            }
            // `None` is upstream's `undefined`: no enclosing def binds the
            // name, so it is no local — the ordinary paths keep it.
            let Some(answer) = self.python_local_class(receiver, r, &mut read)? else {
                return Ok(result);
            };
            result.claimed = true;
            answer
        };
        let declared = matches!(answer, Answer::Declared(_)) && !strict_local;
        match answer {
            Answer::Runtime => result.claimed = true,
            Answer::Class(class) | Answer::Declared(class) => {
                result.claimed = true;
                let mut members = if strict_local {
                    // pythonMethodOnClass: own member, else a declared base's —
                    // read through the file's imports so a base from outside
                    // the project holds none of its methods.
                    self.python_strict_members(&class, method, r, &mut HashSet::new())?
                } else {
                    if let Some(mro) = self.python_mro(&class, 0)? {
                        read.extend(mro.iter().map(|c| c.file_path.clone()));
                    }
                    self.python_members(&class, method, r, &mut HashSet::new())?
                };
                if members.is_empty() && declared {
                    members = self.python_descendant_members(&class, method, r)?;
                }
                let callable: Vec<_> = members
                    .into_iter()
                    .filter(|n| n.kind == "function" || n.kind == "method")
                    .collect();
                if callable.len() == 1 && !self.is_python_property(&callable[0]) {
                    result.target_node_id = Some(callable[0].id.clone());
                }
            }
            _ => {}
        }
        read.remove(&r.file_path);
        result.type_from = read.into_iter().collect();
        result.type_from.sort();
        Ok(result)
    }

    /// Whether the reference sits in a lambda body whose parameters include
    /// `name`: the lambda's own parameter, whose default is that outer value.
    fn python_lambda_parameter_named(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let Some(source) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(tree) = self.parsed_tree(&source, r) else { return Ok(false) };
        let text = source.text();
        let leaf = super::iteration::descendant_for_position(
            tree.root_node(),
            text,
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize),
        );
        let at = leaf.start_byte();
        let mut current = Some(leaf);
        while let Some(node) = current {
            if node.kind() == "lambda"
                && node.child_by_field_name("body").is_some_and(|body| body.start_byte() <= at && at < body.end_byte())
                && node.child_by_field_name("parameters").is_some_and(|params| {
                    super::iteration::named_children(params).into_iter().any(|param| {
                        let named = param.child_by_field_name("name").unwrap_or(param);
                        named.kind() == "identifier" && &text[named.start_byte()..named.end_byte()] == name
                    })
                })
            {
                return Ok(true);
            }
            current = node.parent();
        }
        Ok(false)
    }

    /// Whether the reference `r` sits in a lambda's body (not its parameters or
    /// a default), a deferred read with no graph function node of its own.
    fn python_in_lambda_body(&mut self, r: &ResolveRefIn) -> Res<bool> {
        let Some(source) = self.read_file(&r.file_path) else { return Ok(false) };
        let Some(tree) = self.parsed_tree(&source, r) else { return Ok(false) };
        let text = source.text();
        let leaf = super::iteration::descendant_for_position(
            tree.root_node(),
            text,
            ((r.line - 1).max(0) as usize, r.column.max(0) as usize),
        );
        let at = leaf.start_byte();
        let mut current = Some(leaf);
        while let Some(node) = current {
            if node.kind() == "lambda"
                && node.child_by_field_name("body").is_some_and(|body| body.start_byte() <= at && at < body.end_byte())
            {
                return Ok(true);
            }
            current = node.parent();
        }
        Ok(false)
    }

    /// Whether the def around `r` binds `name` inside a branch, loop, `try` or
    /// `with` before the call: no single value is in effect there, so the
    /// strict read refuses rather than taking the last written assignment.
    fn python_conditional_bindings(&mut self, name: &str, r: &ResolveRefIn) -> Res<bool> {
        let Some(src) = self.read_file(&r.file_path) else { return Ok(false) };
        let file = src.python_file();
        let at = (r.line - 1).max(0) as usize;
        let start = match file.stmt(at) {
            Some((s, code)) if re!(r"^\s*(?:async\s+)?def\b").is_match(code) => Some(*s),
            _ => file.scope.get(at).copied().flatten(),
        };
        let Some(d) = start else { return Ok(false) };
        let assign = Self::cached_regex(&format!(r"^\s*{}\s*(?::[^=]*)?=[^=]", regex::escape(name)))?;
        // One conditional binding beside a `None` placeholder or a later plain
        // assignment still has a value to read. Two conditional bindings in
        // different blocks do not.
        let end = python_scope_end(file, d);
        let mut conditional = 0;
        for i in (d + 1)..=end.min(file.code.len().saturating_sub(1)) {
            let Some(line) = src.get(i) else { break };
            if !assign.is_match(line.trim_start()) { continue; }
            let indent = line.len() - line.trim_start().len();
            let mut j = i;
            let mut inside = false;
            while j > d {
                j -= 1;
                let Some(above) = src.get(j) else { break };
                let trimmed = above.trim_start();
                if trimmed.is_empty() || trimmed.starts_with('#') { continue; }
                if above.len() - trimmed.len() >= indent { continue; }
                if re!(r"^(?:if|elif|else|for|while|try|except|finally|with)\b").is_match(trimmed) {
                    inside = true;
                    break;
                }
            }
            if inside {
                conditional += 1;
                if conditional >= 2 { return Ok(true); }
            }
        }
        Ok(false)
    }

    /// pythonLocalClass (#2583): the project class the local or parameter
    /// `name` holds at `r`, agreed by every binding of it in the binding
    /// def's own body — a parameter annotation, `name: T`, `name = Ctor()`,
    /// `name = factory()`, `name = None` (no evidence). Any other binding —
    /// a `for`/`with`/`except`/comprehension target, a tuple or augmented
    /// assignment, a walrus, an import, `del`, a nested `def` or `class` of
    /// the name, a `nonlocal` write — leaves it unknown, and a contested
    /// one is no class at all. `None` is no local at all — a `global`
    /// binding, a module-level name, or no enclosing def — upstream's
    /// `undefined`, where the ordinary paths keep the call.
    fn python_local_class(&mut self, name: &str, r: &ResolveRefIn, read: &mut HashSet<String>) -> Res<Option<Answer>> {
        let Some(src) = self.read_file(&r.file_path) else {
            return Ok(None);
        };
        let file = src.python_file();
        let at = (r.line - 1).max(0) as usize;
        // pythonBindingScope: the innermost def around the ref that binds the
        // name — a parameter, or any binding in its own body. A `global`
        // binding or no such def is no local.
        let start = match file.stmt(at) {
            Some((s, code)) if re!(r"^\s*(?:async\s+)?def\b").is_match(code) => Some(*s),
            _ => file.scope.get(at).copied().flatten(),
        };
        let mut scope: Option<(usize, Option<Option<String>>, Vec<PyBinding>)> = None;
        for d in file.functions_around(start) {
            let (param, bindings) = python_scope_state(file, &src, d, name)?;
            if param.is_some() {
                scope = Some((d, param, bindings));
                break;
            }
            if bindings.iter().any(|b| matches!(b, PyBinding::Global)) {
                return Ok(None);
            }
            if !bindings.is_empty() {
                scope = Some((d, param, bindings));
                break;
            }
        }
        let Some((d, param, bindings)) = scope else {
            return Ok(None);
        };
        let mut evidence: Vec<Answer> = Vec::new();
        // (evidence index, line, whether it rebinds the name): a bare
        // annotation (`store: B`) does not.
        let mut lines: Vec<(usize, usize, bool)> = Vec::new();
        if let Some(param) = param {
            // A parameter without an annotation is no evidence.
            let Some(ann) = param else {
                return Ok(Some(Answer::Unknown));
            };
            let ev = self.python_ann_answer(&ann, r, read)?;
            if matches!(ev, Answer::Unknown) {
                return Ok(Some(Answer::Unknown));
            }
            lines.push((evidence.len(), 0, true));
            evidence.push(ev);
        }
        for b in &bindings {
            let PyBinding::Assign { ty, value, line } = b else {
                return Ok(Some(Answer::Unknown));
            };
            let value = value.trim();
            let annotation_only = ty.is_some() && value.is_empty();
            let ev = if let Some(ty) = ty {
                // `reg: Base = make()` trusts the annotation; `reg: A = B()` contradicts it.
                if !value.is_empty() && value != "None" {
                    if let Some(ctor) = python_constructor_call(value) {
                        if ctor.rsplit('.').next() != ty.trim().trim_matches(['\'', '"']).rsplit('.').next() {
                            return Ok(Some(Answer::Unknown));
                        }
                    }
                }
                self.python_ann_answer(ty, r, read)?
            } else if value == "None" {
                continue;
            } else if re!(r"^[A-Za-z_]\w*$").is_match(value) {
                // `reg = cap`: what the def's parameter `cap` is annotated
                // with, unless the def binds `cap` anywhere else.
                let (copied_param, copied_bindings) = python_scope_state(file, &src, d, value)?;
                let Some(Some(ann)) = copied_param else {
                    return Ok(Some(Answer::Unknown));
                };
                if !copied_bindings.is_empty() {
                    return Ok(Some(Answer::Unknown));
                }
                self.python_ann_answer(&ann, r, read)?
            } else {
                // A callee the def binds (`Store = load(); reg = Store()`) is no evidence.
                for callee in python_value_callees(value) {
                    let root = callee.split('.').next().unwrap_or(&callee);
                    if self.python_binds_locally(root, &r.file_path, (*line + 1) as i64, false)? {
                        return Ok(Some(Answer::Unknown));
                    }
                }
                self.python_value_text_answer(value, r, read, 0)?
            };
            if matches!(ev, Answer::Absent) {
                continue;
            }
            lines.push((evidence.len(), *line, !annotation_only));
            evidence.push(ev);
        }
        // Nothing here can be a project class: a builtin, or no evidence.
        if !evidence.iter().any(|e| !matches!(e, Answer::Runtime)) {
            return Ok(if evidence.is_empty() { Some(Answer::Unknown) } else { Some(Answer::Runtime) });
        }
        // A def or class of the name nested in the scope binds it there; a
        // def nested in it can rebind the scope's own through `nonlocal`;
        // `with … as (a, name)` binds it in a way bindings don't read.
        let end = python_scope_end(file, d);
        if self.nodes_in_file(&r.file_path)?.iter().any(|n| {
            matches!(n.kind.as_str(), "function" | "method" | "class")
                && n.name == name
                && n.start_line > d as i64 + 1
                && n.end_line <= end as i64 + 1
        }) {
            return Ok(Some(Answer::Unknown));
        }
        for i in d..=end.min(file.code.len().saturating_sub(1)) {
            let Some((_, code)) = file.code.get(i) else { break };
            if !code.contains(name) {
                continue;
            }
            if re!(r"^\s*nonlocal\b").is_match(code) && bare_word(code, name) {
                return Ok(Some(Answer::Unknown));
            }
            if let Some(cap) = re!(r"\bas\s*\(([^)]*)\)").captures(code) {
                if bare_word(&cap[1], name) {
                    return Ok(Some(Answer::Unknown));
                }
            }
        }
        // The fork's receiver-scope rule reads the binding in effect at the
        // call, not every binding the def makes (`store = A(); store = B();
        // store.get()` is B's). A same-line replacement, or a later one a
        // closure's captured cell sees, leaves it unknown; so does a binding
        // shape this read refuses (checked above).
        if self.python_receiver_uncertain(r)? {
            return Ok(Some(Answer::Unknown));
        }
        // A conditional, loop or `try` binding may not run, so no chronological
        // order decides between it and another: `if flag: store = A() else:
        // store = B()` leaves the call unknown, as #2821 refuses.
        let control_bound = |line0: usize| -> bool {
            let Some(binding) = src.get(line0) else { return false };
            let binding_indent = binding.len() - binding.trim_start().len();
            let mut i = line0;
            while i > d {
                i -= 1;
                let Some(line) = src.get(i) else { break };
                let trimmed = line.trim_start();
                if trimmed.is_empty() || trimmed.starts_with('#') { continue; }
                if line.len() - trimmed.len() >= binding_indent { continue; }
                if re!(r"^(?:if|elif|else|for|while|try|except|finally|with)\b").is_match(trimmed) {
                    return true;
                }
            }
            false
        };
        // `PyBinding` lines are zero-based; the ref's line is one-based.
        let at = r.line.max(0) as usize;
        let eligible = |line: &usize, rebinds: &bool| *rebinds && *line < at;
        if let Some((i, line, _)) = lines
            .iter()
            .filter(|(_, line, rebinds)| eligible(line, rebinds))
            .max_by_key(|(_, line, _)| *line)
        {
            // A conflicting set where the binding in effect may not run stays
            // unknown: `store = A(); if flag: store = B(); store.get()`.
            if control_bound(*line) && matches!(agree(evidence.clone()), Answer::Unknown) {
                return Ok(Some(Answer::Unknown));
            }
            return Ok(Some(evidence[*i].clone()));
        }
        // Nothing binds the name before the call: `store.get(); store = A()`
        // raises UnboundLocalError in the def's own body. A nested def that
        // captures the outer name reads it when invoked, which the fork keeps.
        if start == Some(d) {
            return Ok(Some(Answer::Unknown));
        }
        // A lone initialization, later or not, is retained only when it is the
        // def's only evidence.
        Ok(Some(agree(evidence)))
    }

    /// pythonFindMethod's member walk for the strict local path: `member` on
    /// `cls` itself, else on a base the class's file declares — resolved
    /// through that file's imports (`python_bases`), never through the
    /// `extends` edges, so a base spelled from a module outside the project
    /// holds none of its methods.
    fn python_strict_members(
        &mut self,
        cls: &Arc<KNode>,
        member: &str,
        r: &ResolveRefIn,
        seen: &mut HashSet<String>,
    ) -> Res<Vec<Arc<KNode>>> {
        if seen.len() >= 16 || !seen.insert(cls.id.clone()) {
            return Ok(Vec::new());
        }
        if self.python_class_assigns(cls, member)? {
            return Ok(vec![cls.clone()]);
        }
        let own: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(&format!("{}::{}", cls.qualified_name, member))?
            .iter()
            .filter(|n| n.file_path == cls.file_path)
            .cloned()
            .collect();
        if !own.is_empty() {
            return Ok(own);
        }
        let mut out: Vec<Arc<KNode>> = Vec::new();
        for base in self.python_bases(cls, r)? {
            for n in self.python_strict_members(&base, member, r, seen)? {
                if !out.iter().any(|o| o.id == n.id) {
                    out.push(n);
                }
            }
        }
        Ok(out)
    }

    /// pythonAnnotationEvidence → pythonEvidenceClass: `Optional[T]`,
    /// `Union[T, None]`, `T | None` and `"T"` unwrap to `T`; a runtime
    /// container type is 'runtime', anything unreadable is unknown.
    fn python_ann_answer(&mut self, raw: &str, site: &ResolveRefIn, read: &mut HashSet<String>) -> Res<Answer> {
        let mut t = raw.trim().trim_matches(['\'', '"']).trim().to_string();
        for _ in 0..4 {
            if t.is_empty() {
                break;
            }
            let cap = re!(r"^(?:typing\.)?Optional\s*\[\s*(.+)\s*\]$").captures(&t)
                .or_else(|| re!(r"^(?:typing\.)?Union\s*\[\s*([^,\]]+?)\s*,\s*None\s*\]$").captures(&t))
                .or_else(|| re!(r"^(.+?)\s*\|\s*None$").captures(&t))
                .or_else(|| re!(r"^None\s*\|\s*(.+)$").captures(&t));
            let Some(cap) = cap else {
                break;
            };
            t = cap[1].trim().trim_matches(['\'', '"']).trim().to_string();
        }
        let head = t.split('[').next().unwrap_or("").trim().trim_start_matches("typing.");
        if PYTHON_RUNTIME_TYPES.contains(head) {
            return Ok(Answer::Runtime);
        }
        if t.contains('[') || !re!(r"^[A-Za-z_][\w.]*$").is_match(&t) || head == "Any" {
            return Ok(Answer::Unknown);
        }
        self.python_named_answer(&t, site, read)
    }

    /// pythonValueEvidence → pythonEvidenceClass for a whole assigned value:
    /// a conditional's arms must agree; literals and runtime containers are
    /// 'runtime'; `Ctor()` is that class; `factory(...)` resolves by what the
    /// factory produces; anything else is unknown.
    fn python_value_text_answer(
        &mut self,
        raw: &str,
        site: &ResolveRefIn,
        read: &mut HashSet<String>,
        depth: usize,
    ) -> Res<Answer> {
        if depth >= 8 {
            return Ok(Answer::Unknown);
        }
        if let Some((a, b)) = python_conditional_arms(raw) {
            let x = self.python_value_text_answer(&a, site, read, depth + 1)?;
            let y = self.python_value_text_answer(&b, site, read, depth + 1)?;
            return Ok(agree(vec![x, y]));
        }
        let mut awaited = false;
        let mut v = raw.trim();
        if let Some(rest) = v.strip_prefix("await") {
            if rest.starts_with(char::is_whitespace) {
                v = rest.trim();
                awaited = true;
            }
        }
        let Some(shape) = python_expr_shape(v) else {
            return Ok(Answer::Unknown);
        };
        if re!(r"^(?:S(?: S)*|-?\d[\w.]*|True|False|\[\]|\{\}|\(\))$").is_match(&shape) {
            return Ok(if awaited { Answer::Unknown } else { Answer::Runtime });
        }
        if let Some(cap) = re!(r"^([A-Za-z_][\w.]*) ?\(\)$").captures(&shape) {
            let callee = cap[1].to_string();
            if PYTHON_RUNTIME_TYPES.contains(callee.as_str()) {
                return Ok(if awaited { Answer::Unknown } else { Answer::Runtime });
            }
            if callee
                .rsplit('.')
                .next()
                .is_some_and(|leaf| leaf.starts_with(|c: char| c.is_ascii_uppercase()))
            {
                return if awaited {
                    Ok(Answer::Unknown)
                } else {
                    self.python_named_answer(&callee, site, read)
                };
            }
            return self.python_callable_result(&callee, site, read, depth + 1, awaited);
        }
        Ok(Answer::Unknown)
    }
}

/// pythonScopeBindings: `name`'s parameter-annotation state in the def at
/// statement line `d` — None when it is no parameter, Some(None) for a bare
/// or starred one, Some(Some) for `name: T` — plus every binding of the name
/// in the def's own body (a one-line body on the header line counts).
fn python_scope_state(
    file: &PyFile,
    lines: &[String],
    d: usize,
    name: &str,
) -> Res<(Option<Option<String>>, Vec<PyBinding>)> {
    let header = file.stmt(d).map(|(_, h)| h.clone()).unwrap_or_default();
    // Params are read off the raw header — the statement's strings are
    // blanked, so `reg: "T"` reads `reg: "          "` there; offsets match.
    let raw = file
        .stmts
        .iter()
        .position(|(i, _)| *i == d)
        .map(|k| python_raw_stmt(file, lines, k))
        .unwrap_or_else(|| header.clone());
    let params = python_def_params(&header)
        .map(|p| raw.get(p.clone()).map_or_else(|| header[p].to_string(), str::to_string))
        .unwrap_or_default();
    let param = python_param_names(&params)
        .contains(&name)
        .then(|| python_param_ann(&params, name))
        .flatten();
    let mut bindings = Vec::new();
    if let Some(body) = header_colon(&header)
        .map(|at| &header[at + 1..])
        .filter(|b| !b.trim().is_empty())
    {
        bindings.extend(python_stmt_bindings(body, body, name, d)?);
    }
    for (k, (i, code)) in file.stmts.iter().enumerate() {
        if file.scope[*i] != Some(d) || !mentions(code, name) {
            continue;
        }
        let raw = python_raw_stmt(file, lines, k);
        bindings.extend(python_stmt_bindings(code, &raw, name, *i)?);
    }
    Ok((param, bindings))
}

/// The last line inside the `def`/`class` at line `d`, nested bodies
/// included — the range a `nonlocal` or `as (…)` scan must cover.
fn python_scope_end(file: &PyFile, d: usize) -> usize {
    let mut end = d;
    for (i, s) in file.scope.iter().enumerate().skip(d + 1) {
        let mut at = *s;
        while let Some(h) = at {
            if h == d {
                end = i;
                break;
            }
            at = file.scope[h];
        }
    }
    end
}

/// pythonBlankStrings: `line` with the contents of its string literals
/// blanked (quotes kept), so a string never reads as code. A quote left open
/// blanks the rest of the line.
fn python_blank_strings(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c != b'"' && c != b'\'' {
            i += 1;
            continue;
        }
        let triple = bytes.get(i + 1) == Some(&c) && bytes.get(i + 2) == Some(&c);
        let qlen = if triple { 3 } else { 1 };
        let mut j = i + qlen;
        while j < bytes.len() && !line.get(j..).is_some_and(|s| s.starts_with(&line[i..i + qlen])) {
            j += if bytes[j] == b'\\' { 2 } else { 1 };
        }
        for o in out.iter_mut().take(j.min(bytes.len())).skip(i + qlen) {
            *o = b' ';
        }
        i = (j + qlen).min(bytes.len());
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `name: T` or `name: T = d` in a def's parameter list: Some(Some(T)),
/// Some(None) for a bare or starred parameter, None when not a parameter.
/// Commas and the default's `=` are read on a string-blanked copy.
fn python_param_ann(params: &str, name: &str) -> Option<Option<String>> {
    let blank = python_blank_strings(params);
    let mut depth = 0i32;
    let mut from = 0usize;
    let mut parts: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, c) in blank.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(from..i);
                from = i + 1;
            }
            _ => {}
        }
    }
    parts.push(from..blank.len());
    for range in parts {
        let part = &params[range.clone()];
        let Some(cap) = re!(r"^\s*(\*{0,2})([A-Za-z_]\w*)\s*(:?)").captures(part) else {
            continue;
        };
        if &cap[2] != name {
            continue;
        }
        if !cap[1].is_empty() || cap[3].is_empty() {
            return Some(None);
        }
        let at = cap[0].len();
        // The annotation runs up to the default's `=`, outside brackets.
        let mut d = 0i32;
        let mut to = part.len();
        for (i, c) in blank[range.clone()][at..].char_indices() {
            match c {
                '(' | '[' | '{' => d += 1,
                ')' | ']' | '}' => d -= 1,
                '=' if d == 0 => {
                    to = at + i;
                    break;
                }
                _ => {}
            }
        }
        let ann = part[at..to].trim();
        return Some((!ann.is_empty()).then(|| ann.to_string()));
    }
    None
}

/// pythonValueCallees: the names `value` calls as a whole expression or
/// conditional arm — `Client(...)`, `make()` — whose local rebinding would
/// make the evidence wrong.
fn python_value_callees(value: &str) -> Vec<String> {
    if let Some((a, b)) = python_conditional_arms(value.trim()) {
        return [a, b].into_iter().flat_map(|arm| python_value_callees(&arm)).collect();
    }
    let mut v = value.trim();
    if let Some(rest) = v.strip_prefix("await") {
        if rest.starts_with(char::is_whitespace) {
            v = rest.trim();
        }
    }
    python_expr_shape(v)
        .and_then(|shape| re!(r"^([A-Za-z_][\w.]*) ?\(\)$").captures(&shape).map(|c| c[1].to_string()))
        .into_iter()
        .collect()
}

/// pythonConditionalArms: the two arms of a top-level `A if c else B`;
/// keywords inside brackets or strings do not count.
fn python_conditional_arms(raw: &str) -> Option<(String, String)> {
    let mut depth = 0i32;
    let mut if_at: Option<usize> = None;
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        let ch = raw[i..].chars().next().unwrap();
        if c == b'"' || c == b'\'' {
            let triple = bytes.get(i + 1) == Some(&c) && bytes.get(i + 2) == Some(&c);
            let close_len = if triple { 3 } else { 1 };
            let close = &raw[i..i + close_len];
            let mut j = i + close_len;
            while j < bytes.len() && !raw.get(j..).is_some_and(|s| s.starts_with(close)) {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            i = j + close_len;
            continue;
        }
        if matches!(c, b'(' | b'[' | b'{') {
            depth += 1;
        } else if matches!(c, b')' | b']' | b'}') {
            depth -= 1;
        } else if depth == 0 && raw[..i].chars().next_back().is_some_and(|p| p.is_whitespace()) {
            let rest = &raw[i..];
            if if_at.is_none()
                && rest.starts_with("if")
                && rest.len() > 2
                && rest.as_bytes()[2].is_ascii_whitespace()
            {
                if_at = Some(i);
            } else if if_at.is_some()
                && rest.starts_with("else")
                && rest.len() > 4
                && rest.as_bytes()[4].is_ascii_whitespace()
            {
                let at = if_at.unwrap();
                return Some((raw[..at].trim().to_string(), raw[i + 4..].trim().to_string()));
            }
        }
        i += ch.len_utf8();
    }
    None
}

/// pythonExprShape: the top-level shape of a python expression — string
/// literals collapse to `S` and everything inside brackets is dropped, so
/// `Client(a, f(b))` reads `Client()` and `Client() if t else Fake()` keeps
/// its `if`. None when the expression is left open or unrecoverable.
fn python_expr_shape(raw: &str) -> Option<String> {
    python_expr_scan(raw).0
}

/// pythonExprScan: `shape` plus why there is none — 'continues' for an open
/// bracket or triple-quoted string or a trailing `\`, 'broken' for a
/// single-line string left open or a bracket closed that was never opened.
fn python_expr_scan(raw: &str) -> (Option<String>, &'static str) {
    let mut out = String::new();
    let mut depth = 0i32;
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        let ch = raw[i..].chars().next().unwrap();
        if c == b'"' || c == b'\'' {
            let triple = bytes.get(i + 1) == Some(&c) && bytes.get(i + 2) == Some(&c);
            let close_len = if triple { 3 } else { 1 };
            let close = &raw[i..i + close_len];
            let mut j = i + close_len;
            while j < bytes.len() && !raw.get(j..).is_some_and(|s| s.starts_with(close)) {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            if j >= bytes.len() {
                return (None, if triple { "continues" } else { "broken" });
            }
            if depth == 0 {
                let strip = out
                    .chars()
                    .rev()
                    .take(2)
                    .take_while(|k| "rbfuRBFU".contains(*k))
                    .count();
                out.truncate(out.len() - strip);
                out.push('S');
            }
            i = j + close_len;
            continue;
        }
        if matches!(c, b'(' | b'[' | b'{') {
            if depth == 0 {
                out.push(ch);
            }
            depth += 1;
        } else if matches!(c, b')' | b']' | b'}') {
            depth -= 1;
            if depth < 0 {
                return (None, "broken");
            }
            if depth == 0 {
                out.push(ch);
            }
        } else if depth == 0 {
            out.push(ch);
        }
        i += ch.len_utf8();
    }
    let shape = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if depth > 0 || shape.ends_with('\\') {
        return (None, "continues");
    }
    (Some(shape), "none")
}
#[cfg_attr(feature = "addon", napi)]
impl KernelResolver {
    /// Used both by the native pipeline and sync's failed-attribute retry filter.
    #[cfg_attr(feature = "addon", napi)]
    pub fn resolve_python_attribute(&mut self, r: ResolveRefIn) -> Result<PythonAttributeResult> {
        self.python_attribute_verdict(&r).map_err(Error::from)
    }
}
