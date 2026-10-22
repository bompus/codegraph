//! Python attribute types from class bodies, factories and base classes.
//! Syntax trees distinguish executable assignments/returns from strings,
//! nested definitions and incomplete expressions. All evidence names its
//! source files so incremental sync can reopen dependent edges.
use super::*;
use tree_sitter::Node as PyNode;

#[napi(object)]
pub struct PythonAttributeResult {
    /// True for a proven type (including an outside/builtin type with no edge).
    /// False leaves the existing name strategies in charge.
    pub claimed: bool,
    pub target_node_id: Option<String>,
    pub type_from: Vec<String>,
}

#[derive(Clone)]
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
        } else {
            let Some((head, attr)) = receiver
                .split_once('.')
                .filter(|(_, a)| simple_name(a) && !a.contains('.'))
            else {
                return Ok(result);
            };
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
        };
        let declared = matches!(answer, Answer::Declared(_));
        match answer {
            Answer::Runtime => result.claimed = true,
            Answer::Class(class) | Answer::Declared(class) => {
                result.claimed = true;
                if let Some(mro) = self.python_mro(&class, 0)? {
                    read.extend(mro.iter().map(|c| c.file_path.clone()));
                }
                let mut members = self.python_members(&class, method, r, &mut HashSet::new())?;
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
}
#[napi]
impl KernelResolver {
    /// Used both by the native pipeline and sync's failed-attribute retry filter.
    #[napi]
    pub fn resolve_python_attribute(&mut self, r: ResolveRefIn) -> Result<PythonAttributeResult> {
        self.python_attribute_verdict(&r).map_err(Error::from)
    }
}
