//! Pascal scope resolution — port of upstream's `resolution/pascal-scope.ts`.
//!
//! Covers three upstream features sharing one scope model:
//!
//! - Names written without parentheses (#2514): a parameterless routine is
//!   called that way in Delphi — `Before;`, `Result := FProvider.Value + 1`,
//!   `if Ready then` — and the same spelling reads a field, a property, a
//!   variable or a constant. The extractor records such a name as a
//!   `references` ref named `{mode}.Name` or `{mode}.Receiver.Name`:
//!     - `{call}` — a statement on its own, which only a routine can be;
//!     - `{read}` — a name in an expression or an argument, where a function
//!       (or a constructor) is called for its value and a procedure, which
//!       has none, can only be a method pointer;
//!     - `{ref}` — the value given to an event (`OnClick := Handler`), a
//!       method pointer;
//!     - `{recv}` — a receiver (`GetList` in `GetList.Count`, `B` in
//!       `A.B.C`) or an indexed value, which only a function can be.
//!
//!   Written without its arguments, a routine that needs some is a method
//!   pointer too, in any of them. The routine's class follows an `@`
//!   (`{call@TConsumer}.Before`), because the reference's own node is the
//!   file when an implementation matched no declaration. A receiver the
//!   routine declares itself is written as its declared type after a `:`
//!   (`{read}.:TProvider.Value`).
//!
//! - Calls to the names Delphi's compiler and System unit give meaning to
//!   (`SizeOf`, `TypeInfo`, `length(S)`, `Pointer(P)`), whatever their case:
//!   only a declaration in the call's scope shadows them (#2803).
//!
//! - Bare calls to routines nested where the call is made (`Inner(1)` in the
//!   routine that declares `procedure Inner(N: Integer);`), and calls made
//!   inside a nested routine, which reach the rest of that routine's scope
//!   the same way (#2810).
//!
//! Every lookup follows Delphi's scope rules — the routine's class and its
//! ancestors, then the unit, then the units it uses, last first. A unit is
//! its own file plus the include files it pulls in (`{$I name.inc}`). The
//! first declaration of the name in that order decides: when it is data (a
//! field, property, variable, constant, enum value or type), nothing is
//! linked unless the data's declared type is a routine's.

use super::*;
use tables::{PASCAL_SYSTEM_NAMES, RTL_PROCEDURAL_TYPES};

const PARENLESS: &str = r"^\{(call|read|ref|recv)(?:@([^}]+))?\}\.(.+)$";
const CLASS_KINDS: &[&str] = &["class", "struct", "interface"];
/// What a unit declares by name at its top level.
const UNIT_KINDS: &[&str] = &[
    "function", "variable", "constant", "class", "struct", "interface",
    "enum", "type_alias", "enum_member",
];
/// What a class declares as its members.
const MEMBER_KINDS: &[&str] =
    &["method", "field", "property", "constant", "variable"];
/// Declarations that hold a value rather than run code.
const DATA_KINDS: &[&str] =
    &["field", "property", "variable", "constant", "enum_member"];
/// What a Pascal typecast's name resolves to (`TFoo(x)` casts, it doesn't
/// call).
pub(super) const CAST_TARGET_KINDS: &[&str] =
    &["class", "struct", "interface", "enum", "type_alias"];
const PASCAL_SOURCE: &str = r"(?i)\.(?:pas|dpr|dpk|lpr|inc)$";
/// A routine type written out: `procedure(...) of object`, `function: T`,
/// `reference to procedure`.
const PROCEDURAL_TYPE: &str = r"(?i)^(?:reference\s+to\s+)?(?:procedure|function)\b";
const MAX_HIERARCHY_DEPTH: usize = 8;

#[derive(Clone)]
struct PasFound {
    node: Arc<KNode>,
    resolved_by: &'static str,
    confidence: f64,
}

/// What a dotted name's first link is: a value of a known class (a local's
/// declared type, written `:TFoo`), a class itself, or a unit.
struct PasHead {
    ty: Option<Arc<KNode>>,
    unit_file: Option<String>,
}

/// A routine header or scope boundary found while scanning a file once
/// (headerClassAt's per-file table).
struct PasHeader {
    line: i64,
    class_qn: Option<String>,
    routine: bool,
}

/// What scope lookups need of a file's nodes, read once: its classes, its
/// `uses` in order, how far its own routines are indented at most.
struct PasOutline {
    classes: Vec<Arc<KNode>>,
    imports: Vec<Arc<KNode>>,
    /// In a unit whose `unit` header the parser lost, the members of a class
    /// it couldn't read come out as routines of the unit, indented deeper
    /// than the unit's own; i64::MAX elsewhere (any column).
    routine_column: i64,
}

/// The project's files by lowercased name, and its Pascal sources.
struct PasProjectFiles {
    by_name: HashMap<String, Vec<String>>,
    pascal: Vec<String>,
}

/// pascal-scope.ts's per-context memos, held for the resolver's run.
#[derive(Default)]
pub(super) struct PasMemo {
    in_scope: HashMap<String, Option<PasFound>>,
    class_by_qn: HashMap<String, Option<Arc<KNode>>>,
    ancestors: HashMap<String, Rc<Vec<Arc<KNode>>>>,
    used_files: HashMap<String, Vec<String>>,
    class_by_name: HashMap<String, Option<Arc<KNode>>>,
    units_by_name: Option<Rc<HashMap<String, Vec<String>>>>,
    generic_types: Option<Rc<HashMap<String, Vec<Arc<KNode>>>>>,
    project_files: Option<Rc<PasProjectFiles>>,
    includes: HashMap<String, Rc<Vec<String>>>,
    includers: HashMap<String, Option<String>>,
    own_files: HashMap<String, Rc<Vec<String>>>,
    unit_file_sets: HashMap<String, Rc<Vec<String>>>,
    unit_files: HashMap<String, Option<String>>,
    hierarchy: HashMap<String, Option<Arc<KNode>>>,
    member_types: HashMap<String, Option<Arc<KNode>>>,
    keywords: HashMap<String, Option<String>>,
    procedural: HashMap<String, bool>,
    header_classes: HashMap<String, Rc<Vec<PasHeader>>>,
    outlines: HashMap<String, Rc<PasOutline>>,
    condition_depths: HashMap<String, Rc<Vec<i64>>>,
    nested_in_scope: HashMap<String, Option<PasFound>>,
    routine_nodes: HashMap<String, Option<Arc<KNode>>>,
    routine_qns: HashMap<String, bool>,
    supers: HashMap<String, Rc<Vec<String>>>,
}

impl KernelResolver {
    // ------------------------------------------------------------------
    // Ref matchers
    // ------------------------------------------------------------------

    /// isPascalParenlessRef — a pascal `references` ref named `{mode}.path`.
    pub(super) fn is_pascal_parenless_ref(&self, r: &ResolveRefIn) -> bool {
        r.language == "pascal" && r.reference_kind == "references"
            && re!(PARENLESS).is_match(&r.reference_name)
    }

    /// matchPascalParenlessRef — the routine a paren-less Pascal name
    /// reaches in its scope, or None (see the file comment).
    pub(super) fn resolve_pascal_parenless(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<Option<ResolveOutcome>> {
        if !self.is_pascal_parenless_ref(r) {
            return Ok(None);
        }
        let m = re!(PARENLESS).captures(&r.reference_name).unwrap();
        let mode = m[1].to_string();
        let path: Vec<String> =
            m[3].split('.').map(|s| s.to_string()).collect();
        // Most such names read data, often the framework's: a name no
        // Pascal routine has is settled here, before any lookup in scope.
        let last = path[path.len() - 1].to_lowercase();
        let routine_exists = self.nodes_by_lower_name(&last)?.iter().any(|n| {
            n.language == "pascal"
                && matches!(n.kind.as_str(), "method" | "function")
        });
        if !routine_exists {
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        let cls = if let Some(qn) = m.get(2) {
            self.pascal_class_by_qn(qn.as_str(), &r.file_path)?
        } else {
            match self.node_by_id(&r.from_node_id)? {
                Some(routine) => self.pascal_class_of_routine(&routine)?,
                None => None,
            }
        };
        let found = if path.len() == 1 {
            self.pascal_name_in_scope(&path[0], cls.as_deref(), r)?
        } else {
            self.pascal_member_through_path(&path, cls.as_deref(), r)?
        };
        let Some(found) = found else {
            return Ok(Some(ResolveOutcome::unresolved()));
        };
        // Written without arguments, a routine that needs some can only be
        // a method pointer — save a constructor, whose name then means an
        // inherited one outside the project (`TObject.Create`) — and so is
        // a procedure read for a value, and an event's method; any other
        // runs.
        let needs_args = pascal_needs_arguments(&found.node);
        if needs_args && self.pascal_routine_keyword(&found.node)? == Some("constructor".into()) {
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        let pointer = mode == "ref" || needs_args
            || (mode != "call" && !self.pascal_yields_value(&found.node)?);
        // A statement runs its routine, and a receiver is a routine's value.
        if pointer && (mode == "call" || mode == "recv") {
            return Ok(Some(ResolveOutcome::unresolved()));
        }
        let edge_kind =
            if pointer { "references".to_string() } else { "calls".to_string() };
        let mut out = self.finish_pre_framework(
            r,
            KCand {
                node: found.node,
                confidence: found.confidence,
                resolved_by: found.resolved_by,
            },
        )?;
        if out.status == "resolved" {
            out.edge_kind = Some(edge_kind);
            // A method pointer is a function used as a value, as `@Handler`
            // is (#756).
            if pointer { out.fn_ref = Some(true); }
        }
        Ok(Some(out))
    }

    /// isPascalSystemCall — a bare Pascal call to one of the System unit's
    /// names (see PASCAL_SYSTEM_NAMES), whatever its case.
    pub(super) fn is_pascal_system_call(&mut self, r: &ResolveRefIn) -> Res<bool> {
        Ok(r.language == "pascal"
            && r.reference_kind == "calls"
            && PASCAL_SYSTEM_NAMES.contains(r.reference_name.to_lowercase().as_str())
            && self.pascal_is_bare_call(r)?)
    }

    /// matchPascalSystemCall — a call to one of the System unit's names is
    /// the System unit's, which the project doesn't hold, unless a
    /// declaration in the call's scope shadows it: a member of the class
    /// (`Delete(I)` in a list class), a routine of the unit or the units it
    /// uses. A project type of that name is a typecast. Data of that name
    /// runs only when it holds a routine (see isProceduralData). A unit that
    /// declares one under a condition stands in for compilers without it
    /// (`{$IFNDEF UNICODE} UnicodeString = WideString; {$ENDIF}`): the code
    /// is most likely built with one that has it.
    pub(super) fn match_pascal_system_call(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<ResolveOutcome> {
        let cls = self.pascal_class_of_call(r)?;
        let decl = self.pascal_declaration_in_scope(
            &r.reference_name.to_lowercase(),
            cls.as_deref(),
            r,
        )?;
        let Some(decl) = decl else {
            return Ok(ResolveOutcome::unresolved());
        };
        if DATA_KINDS.contains(&decl.node.kind.as_str())
            && !self.pascal_is_procedural_data(&decl.node)?
        {
            return Ok(ResolveOutcome::unresolved());
        }
        if !decl.node.qualified_name.contains("::")
            && self.pascal_declared_under_condition(&decl.node)?
        {
            return Ok(ResolveOutcome::unresolved());
        }
        self.finish_pre_framework(
            r,
            KCand {
                node: decl.node,
                confidence: decl.confidence,
                resolved_by: decl.resolved_by,
            },
        )
    }

    /// matchPascalNestedCall — a bare Pascal call (`Inner(1)`) to a routine
    /// nested where the call is made (see nestedRoutineInScope). Made inside
    /// a nested routine, it reaches the rest of that routine's scope the
    /// same way: the class of the method it is nested in, its unit, the
    /// units it uses. None when that scope holds no routine of the name,
    /// for the other strategies to resolve.
    pub(super) fn resolve_pascal_nested_call(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<Option<ResolveOutcome>> {
        if r.language != "pascal"
            || r.reference_kind != "calls"
            || !self.pascal_is_bare_call(r)?
        {
            return Ok(None);
        }
        let lower = r.reference_name.to_lowercase();
        let mut found = self.pascal_nested_routine_in_scope(&lower, r)?;
        if found.is_none() {
            let Some(caller) = self.pascal_routine_node(&r.from_node_id)? else {
                return Ok(None);
            };
            if !self.pascal_is_nested_routine(&caller)? {
                return Ok(None);
            }
            let cls = self.pascal_class_of_call(r)?;
            let Some(decl) =
                self.pascal_declaration_in_scope(&lower, cls.as_deref(), r)?
            else {
                return Ok(None);
            };
            if !pascal_is_routine(&decl.node)
                && !(DATA_KINDS.contains(&decl.node.kind.as_str())
                    && self.pascal_is_procedural_data(&decl.node)?)
            {
                return Ok(None);
            }
            found = Some(decl);
        }
        let found = found.unwrap();
        self.finish_pre_framework(
            r,
            KCand {
                node: found.node,
                confidence: found.confidence,
                resolved_by: found.resolved_by,
            },
        )
        .map(Some)
    }

    /// gatePascalCall — what a Pascal call another strategy resolved stands
    /// for, when it landed on data or on a routine nested in another. Runs
    /// on every resolved pascal `calls` verdict, after resolve_ref_inner.
    ///
    /// A call resolved to data — a field, a property, a variable, a
    /// constant — runs what the data holds, so it stands only for data
    /// whose declared type is a routine's. A bare name is what its scope
    /// declares (`FOnChange(Self)` in the class that declares its own
    /// `FOnChange`): the routine or the routine-holding data there,
    /// whichever namesake the strategy chose. Any other is a namesake:
    /// `length(S)` was a record's `length` field, `Pointer(P)` a property
    /// named `Pointer`. A name the calling routine declares itself, as a
    /// parameter, a local or a nested routine, is that declaration, which
    /// the graph doesn't hold. A name its scope doesn't declare at all is a
    /// `with` object's member (`with OCI do HandleAlloc(...)`) or a
    /// declaration the parse put in the wrong place: it stands for the one
    /// routine-holding field or variable the project declares by that name.
    pub(super) fn gate_pascal_call(
        &mut self,
        out: ResolveOutcome,
        r: &ResolveRefIn,
    ) -> Res<ResolveOutcome> {
        let Some(tid) = &out.target_node_id else {
            return Ok(out);
        };
        let Some(target) = self.node_by_id(tid)? else {
            return Ok(out);
        };
        if DATA_KINDS.contains(&target.kind.as_str()) {
            return self.gate_pascal_data_call(&target, out, r);
        }
        if self.pascal_is_nested_routine(&target)? {
            return self.gate_pascal_nested_target(out, r);
        }
        Ok(out)
    }

    /// See gate_pascal_call: a call resolved to data.
    fn gate_pascal_data_call(
        &mut self,
        target: &KNode,
        out: ResolveOutcome,
        r: &ResolveRefIn,
    ) -> Res<ResolveOutcome> {
        if !self.pascal_is_bare_call(r)? {
            return Ok(
                if target.language == "pascal" && self.pascal_is_procedural_data(target)? {
                    out
                } else {
                    ResolveOutcome::unresolved()
                },
            );
        }
        let lower = r.reference_name.to_lowercase();
        if self.pascal_declared_in_routine(&lower, r)? {
            return Ok(ResolveOutcome::unresolved());
        }
        let cls = self.pascal_class_of_call(r)?;
        let decl = self.pascal_declaration_in_scope(&lower, cls.as_deref(), r)?;
        let Some(decl) = decl else {
            let only = target.language == "pascal"
                && self
                    .pascal_named(&lower)?
                    .iter()
                    .all(|n| n.id == target.id || n.kind == "import");
            return Ok(
                if only && self.pascal_is_procedural_data(target)? {
                    out
                } else {
                    ResolveOutcome::unresolved()
                },
            );
        };
        if !pascal_is_routine(&decl.node)
            && !(DATA_KINDS.contains(&decl.node.kind.as_str())
                && self.pascal_is_procedural_data(&decl.node)?)
        {
            return Ok(ResolveOutcome::unresolved());
        }
        if Some(decl.node.id.as_str()) == out.target_node_id.as_deref() {
            return Ok(out);
        }
        Ok(retarget(out, &decl))
    }

    /// A Pascal call another strategy resolved to a routine nested in some
    /// other routine, which nothing outside that routine can call: a bare
    /// name is then what the call's scope declares, a routine or data
    /// holding one (mORMot's `StrLen` variable), and any other call links
    /// nothing.
    fn gate_pascal_nested_target(
        &mut self,
        out: ResolveOutcome,
        r: &ResolveRefIn,
    ) -> Res<ResolveOutcome> {
        if !self.pascal_is_bare_call(r)? {
            return Ok(ResolveOutcome::unresolved());
        }
        let lower = r.reference_name.to_lowercase();
        let cls = self.pascal_class_of_call(r)?;
        let Some(decl) = self.pascal_declaration_in_scope(&lower, cls.as_deref(), r)?
        else {
            return Ok(ResolveOutcome::unresolved());
        };
        if !pascal_is_routine(&decl.node)
            && !(DATA_KINDS.contains(&decl.node.kind.as_str())
                && self.pascal_is_procedural_data(&decl.node)?)
        {
            return Ok(ResolveOutcome::unresolved());
        }
        if Some(decl.node.id.as_str()) == out.target_node_id.as_deref() {
            return Ok(out);
        }
        Ok(retarget(out, &decl))
    }

    /// isBareCall — whether a Pascal call named by a bare name is written
    /// bare: its expression starts with the name (`Reset(F)`). The last
    /// call of a chain is named bare too (`Reset` in `GetKey(P).Reset`),
    /// and its scope is its receiver's type.
    fn pascal_is_bare_call(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if !re!(r"^[A-Za-z_]\w*$").is_match(&r.reference_name) {
            return Ok(false);
        }
        let Some(source) = self.read_file(&r.file_path) else {
            return Ok(false);
        };
        let line = source
            .get((r.line - 1).max(0) as usize)
            .cloned()
            .unwrap_or_default();
        let tail = line.get(r.column.max(0) as usize..).unwrap_or("");
        Ok(re!(r"^[A-Za-z_]\w*")
            .find(tail)
            .is_some_and(|m| m.as_str().eq_ignore_ascii_case(&r.reference_name)))
    }

    // ------------------------------------------------------------------
    // Scope lookup
    // ------------------------------------------------------------------

    /// A bare name, looked up the way Delphi looks it up from a routine of
    /// class `cls` (None: a free routine): a routine, or None.
    fn pascal_name_in_scope(
        &mut self,
        name: &str,
        cls: Option<&KNode>,
        r: &ResolveRefIn,
    ) -> Res<Option<PasFound>> {
        let decl =
            self.pascal_declaration_in_scope(&name.to_lowercase(), cls, r)?;
        Ok(decl.filter(|f| pascal_is_routine(&f.node)))
    }

    /// declarationInScope — the first declaration a bare name reaches from
    /// a routine of class `cls` (None: a free routine), whatever it
    /// declares: a routine nested in the calling routine or one around it,
    /// a member of the class or its ancestors, then a declaration of the
    /// unit, then of the units it uses, last first. None when the project
    /// declares none of them, as for the System unit's names.
    fn pascal_declaration_in_scope(
        &mut self,
        lower: &str,
        cls: Option<&KNode>,
        r: &ResolveRefIn,
    ) -> Res<Option<PasFound>> {
        if let Some(nested) = self.pascal_nested_routine_in_scope(lower, r)? {
            return Ok(Some(nested));
        }
        let key = format!(
            "{}|{}|{}",
            cls.map(|c| c.id.as_str()).unwrap_or(""),
            r.file_path,
            lower
        );
        if let Some(hit) = self.pascal.in_scope.get(&key) {
            return Ok(hit.clone());
        }
        let mut found: Option<PasFound> = None;
        let member = match cls {
            Some(cls) => self.pascal_member_in_hierarchy(cls, lower)?,
            None => None,
        };
        if let Some(member) = member {
            found = Some(PasFound {
                node: member,
                resolved_by: "instance-method",
                confidence: 0.9,
            });
        } else if let Some(own) =
            self.pascal_own_unit_declaration(&r.file_path, lower)?
        {
            found = Some(PasFound {
                node: own,
                resolved_by: "exact-match",
                confidence: 0.9,
            });
        } else {
            for file in
                self.pascal_used_unit_files(&r.file_path)?.into_iter().rev()
            {
                if let Some(decl) =
                    self.pascal_unit_declaration(&file, lower, &r.file_path)?
                {
                    found = Some(PasFound {
                        node: decl,
                        resolved_by: "import",
                        confidence: 0.85,
                    });
                    break;
                }
            }
        }
        self.pascal.in_scope.insert(key, found.clone());
        Ok(found)
    }

    /// nestedRoutineInScope — the routine nested in the calling routine, or
    /// in one around it, that a bare name there names (`procedure Inner;`
    /// between `procedure TFoo.Bar;` and its `begin`,
    /// `TFoo::Bar::Inner`): the innermost one declared above the call, the
    /// calling routine itself among them. It comes before anything of the
    /// class or the unit. None when none is.
    fn pascal_nested_routine_in_scope(
        &mut self,
        lower: &str,
        r: &ResolveRefIn,
    ) -> Res<Option<PasFound>> {
        // Most names no nested routine has: settled before any other lookup.
        let named = self.pascal_named(lower)?;
        if !named
            .iter()
            .any(|n| n.kind == "function" && n.qualified_name.contains("::"))
        {
            return Ok(None);
        }
        let key = format!("{}|{}", r.from_node_id, lower);
        if let Some(hit) = self.pascal.nested_in_scope.get(&key) {
            return Ok(hit.clone());
        }
        let mut found: Option<PasFound> = None;
        if let Some(caller) = self.pascal_routine_node(&r.from_node_id)? {
            if caller.language == "pascal" {
                let chain = caller.qualified_name.to_lowercase();
                let mut best: Option<Arc<KNode>> = None;
                let mut depth = -1i64;
                for n in &named {
                    if n.file_path != caller.file_path
                        || n.start_line > r.line
                        || !self.pascal_is_nested_routine(n)?
                    {
                        continue;
                    }
                    let outer = pascal_owner_of(n);
                    // Nested in the caller, or in a routine the caller is
                    // nested in.
                    if chain != outer
                        && !chain.starts_with(&format!("{outer}::"))
                    {
                        continue;
                    }
                    if outer.len() as i64 > depth
                        || (outer.len() as i64 == depth
                            && n.start_line
                                < best.as_ref().map(|b| b.start_line).unwrap_or(i64::MAX))
                    {
                        best = Some(n.clone());
                        depth = outer.len() as i64;
                    }
                }
                if let Some(best) = best {
                    found = Some(PasFound {
                        node: best,
                        resolved_by: "exact-match",
                        confidence: 0.95,
                    });
                }
            }
        }
        self.pascal.nested_in_scope.insert(key, found.clone());
        Ok(found)
    }

    /// isNestedRoutine — whether a node is a routine nested in another: a
    /// Pascal function qualified under a routine, which no code outside the
    /// one it is nested in can call.
    fn pascal_is_nested_routine(&mut self, node: &KNode) -> Res<bool> {
        if node.kind != "function" || node.language != "pascal" {
            return Ok(false);
        }
        let Some(cut) = node.qualified_name.rfind("::") else {
            return Ok(false);
        };
        let outer = node.qualified_name[..cut].to_string();
        if let Some(hit) = self.pascal.routine_qns.get(&outer) {
            return Ok(*hit);
        }
        let routine = self
            .nodes_by_qualified_name(&outer)?
            .iter()
            .any(|n| n.language == "pascal" && pascal_is_routine(n));
        self.pascal.routine_qns.insert(outer, routine);
        Ok(routine)
    }

    /// routineNode — the routine a reference comes from, or None when it
    /// comes from a file or anything else.
    fn pascal_routine_node(&mut self, id: &str) -> Res<Option<Arc<KNode>>> {
        if let Some(hit) = self.pascal.routine_nodes.get(id) {
            return Ok(hit.clone());
        }
        let node = self.node_by_id(id)?;
        let routine = node.filter(|n| pascal_is_routine(n));
        self.pascal
            .routine_nodes
            .insert(id.to_string(), routine.clone());
        Ok(routine)
    }

    /// declaredInRoutine — whether the routine making a call declares the
    /// called name itself, as a parameter, a local or a nested routine
    /// (`function Hash(Item: Pointer; Hasher: THasher)` calling
    /// `Hasher(0, Item, 4)`): from the routine's header down to the call.
    fn pascal_declared_in_routine(
        &mut self,
        lower: &str,
        r: &ResolveRefIn,
    ) -> Res<bool> {
        let Some(source) = self.read_file(&r.file_path) else {
            return Ok(false);
        };
        // The header in the text: a method's node may start at its
        // declaration in the class, above the fields and the unit's
        // variables.
        let from = self.pascal_routine_header_at(&r.file_path, r.line)?;
        let mentions = Self::cached_regex(&format!(r"(?i)\b{}\b", regex::escape(lower)))?;
        let declares_routine = Self::cached_regex(&format!(
            r"(?i)\b(?:procedure|function)\s+{}\b",
            regex::escape(lower)
        ))?;
        let declares_var = Self::cached_regex(&format!(
            r"(?i)\b{}\b\s*(?:,\s*[A-Za-z_]\w*\s*)*:",
            regex::escape(lower)
        ))?;
        let strip = re!(r"//.*$|\{[^}]*\}|'[^']*'");
        for i in from.max(1)..=r.line.min(source.len() as i64) {
            let text = &source[(i - 1) as usize];
            if !mentions.is_match(text) {
                continue;
            }
            let stripped = strip.replace_all(text, " ");
            // `name:` declares a variable or parameter; `name :=` assigns.
            if declares_routine.is_match(&stripped)
                || declares_var.find(&stripped).is_some_and(|m| {
                    !stripped[m.end()..].starts_with('=')
                })
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The line of the nearest routine header at column 0 at or above a
    /// line, else the line itself.
    fn pascal_routine_header_at(
        &mut self,
        file_path: &str,
        line: i64,
    ) -> Res<i64> {
        let headers = self.pascal_header_classes(file_path)?;
        let mut found = line;
        for h in headers.iter() {
            if h.line > line {
                break;
            }
            if h.routine {
                found = h.line;
            }
        }
        Ok(found)
    }

    /// The class whose method makes a call: the one the nearest method
    /// header above the call names in the source text
    /// (`procedure TFoo.Bar;`), which holds where the tree had errors and
    /// gave the code to the file, or to a routine that swallowed the ones
    /// after it; else the reference's method's class.
    fn pascal_class_of_call(
        &mut self,
        r: &ResolveRefIn,
    ) -> Res<Option<Arc<KNode>>> {
        if let Some(qn) = self.pascal_header_class_at(&r.file_path, r.line)? {
            if let Some(cls) = self.pascal_class_by_qn(&qn, &r.file_path)? {
                return Ok(Some(cls));
            }
        }
        match self.node_by_id(&r.from_node_id)? {
            Some(routine) if routine.kind == "method" => {
                self.pascal_class_of_routine(&routine)
            }
            _ => Ok(None),
        }
    }

    /// headerClassAt — the class the nearest routine header at or above a
    /// line names (`procedure TFoo.Bar;` → `TFoo`), or None when it is a
    /// free routine's, or the unit's code resumes there: after an `end;`
    /// that starts its line, or at `initialization`.
    fn pascal_header_class_at(
        &mut self,
        file_path: &str,
        line: i64,
    ) -> Res<Option<String>> {
        let headers = self.pascal_header_classes(file_path)?;
        let mut hit: Option<String> = None;
        for h in headers.iter() {
            if h.line <= line {
                hit = h.class_qn.clone();
            } else {
                break;
            }
        }
        Ok(hit)
    }

    /// The per-file table of routine headers and scope boundaries.
    fn pascal_header_classes(
        &mut self,
        file_path: &str,
    ) -> Res<Rc<Vec<PasHeader>>> {
        if let Some(hit) = self.pascal.header_classes.get(file_path) {
            return Ok(hit.clone());
        }
        let mut headers = Vec::new();
        if let Some(source) = self.read_file(file_path) {
            let header = re!(
                r"(?i)^(?:class\s+)?(?:procedure|function|constructor|destructor|operator)\s+([A-Za-z_][\w.<>, ]*?)\s*[(:;]"
            );
            for (i, text) in source.iter().enumerate() {
                // Each starts its line with one of c, d, e, f, i, o, p
                // (either case).
                let first = text.as_bytes().first().map(|b| b | 0x20);
                if !matches!(
                    first,
                    Some(0x63 | 0x64 | 0x65 | 0x66 | 0x69 | 0x6f | 0x70)
                ) {
                    continue;
                }
                if re!(r"(?i)^(?:end;|(?:initialization|finalization)\b)")
                    .is_match(text)
                {
                    headers.push(PasHeader {
                        line: i as i64 + 1,
                        class_qn: None,
                        routine: false,
                    });
                    continue;
                }
                let Some(m) = header.captures(text) else {
                    continue;
                };
                let cleaned = re!(r"<[^>]*>").replace_all(&m[1], "");
                let cleaned = re!(r"\s+").replace_all(&cleaned, "");
                let parts: Vec<&str> = cleaned.split('.').collect();
                let class_qn = if parts.len() > 1 {
                    Some(parts[..parts.len() - 1].join("::"))
                } else {
                    None
                };
                headers.push(PasHeader {
                    line: i as i64 + 1,
                    class_qn,
                    routine: true,
                });
            }
        }
        let rc = Rc::new(headers);
        self.pascal
            .header_classes
            .insert(file_path.to_string(), rc.clone());
        Ok(rc)
    }

    /// declaredUnderCondition — whether a declaration is inside an
    /// `{$IFDEF}`, `{$IFNDEF}`, `{$IF}` or `{$IFOPT}` block, or its
    /// `{$ELSE}`.
    fn pascal_declared_under_condition(&mut self, node: &KNode) -> Res<bool> {
        let depths = self.pascal_condition_depths(&node.file_path)?;
        let base = depths
            .get((node.start_line - 1).max(0) as usize)
            .copied()
            .unwrap_or(0);
        let line = self
            .read_file(&node.file_path)
            .and_then(|s| {
                s.get((node.start_line - 1).max(0) as usize).cloned()
            })
            .unwrap_or_default();
        let head = line
            .get(..node.start_column.max(0) as usize)
            .unwrap_or(&line);
        Ok(pascal_condition_depth_after(head, base) > 0)
    }

    /// The depth of conditional blocks at each line of a file, once.
    fn pascal_condition_depths(
        &mut self,
        file_path: &str,
    ) -> Res<Rc<Vec<i64>>> {
        if let Some(hit) = self.pascal.condition_depths.get(file_path) {
            return Ok(hit.clone());
        }
        let mut depths = Vec::new();
        if let Some(source) = self.read_file(file_path) {
            let mut depth = 0i64;
            for text in source.iter() {
                depths.push(depth);
                if text.contains("{$") {
                    depth = pascal_condition_depth_after(text, depth);
                }
            }
        }
        let rc = Rc::new(depths);
        self.pascal
            .condition_depths
            .insert(file_path.to_string(), rc.clone());
        Ok(rc)
    }

    /// A dotted name: the method its receiver's declared type has.
    fn pascal_member_through_path(
        &mut self,
        path: &[String],
        cls: Option<&KNode>,
        r: &ResolveRefIn,
    ) -> Res<Option<PasFound>> {
        let Some(head) = self.pascal_head_of(&path[0], cls, r)? else {
            return Ok(None);
        };
        let mut ty: Option<Arc<KNode>>;
        let mut rest: &[String] = &path[1..];
        if let Some(unit_file) = head.unit_file {
            // `UnitName.Routine`, `UnitName.GlobalVar.Method`.
            let Some(decl) = self.pascal_unit_declaration(
                &unit_file,
                &rest[0].to_lowercase(),
                &r.file_path,
            )?
            else {
                return Ok(None);
            };
            if rest.len() == 1 {
                return Ok(if pascal_is_routine(&decl) {
                    Some(PasFound {
                        node: decl,
                        resolved_by: "import",
                        confidence: 0.9,
                    })
                } else {
                    None
                });
            }
            ty = self.pascal_type_of_member(&decl)?;
            rest = &rest[1..];
        } else {
            ty = head.ty;
        }
        for link in &rest[..rest.len() - 1] {
            let member = match ty.as_deref() {
                Some(t) => {
                    self.pascal_member_in_hierarchy(t, &link.to_lowercase())?
                }
                None => None,
            };
            let Some(member) = member else { return Ok(None) };
            ty = self.pascal_type_of_member(&member)?;
        }
        let member = match ty.as_deref() {
            Some(t) => self
                .pascal_member_in_hierarchy(t, &rest[rest.len() - 1].to_lowercase())?,
            None => None,
        };
        Ok(member.and_then(|m| {
            if pascal_is_routine(&m) {
                Some(PasFound {
                    node: m,
                    resolved_by: "instance-method",
                    confidence: 0.9,
                })
            } else {
                None
            }
        }))
    }

    /// headOf — what a dotted name's first link is: a value of a known
    /// class (a local's declared type, written `:TFoo`), a class itself,
    /// or a unit.
    fn pascal_head_of(
        &mut self,
        name: &str,
        cls: Option<&KNode>,
        r: &ResolveRefIn,
    ) -> Res<Option<PasHead>> {
        if let Some(ty_name) = name.strip_prefix(':') {
            return Ok(Some(PasHead {
                ty: self.pascal_class_named(ty_name, &r.file_path)?,
                unit_file: None,
            }));
        }
        let lower = name.to_lowercase();
        if lower == "self" {
            return Ok(cls.map(|cls| PasHead {
                ty: Some(Arc::new(cls.clone())),
                unit_file: None,
            }));
        }
        if let Some(cls) = cls {
            if let Some(member) = self.pascal_member_in_hierarchy(cls, &lower)? {
                return Ok(Some(PasHead {
                    ty: self.pascal_type_of_member(&member)?,
                    unit_file: None,
                }));
            }
        }
        let mut decl = self.pascal_own_unit_declaration(&r.file_path, &lower)?;
        if decl.is_none() {
            for file in
                self.pascal_used_unit_files(&r.file_path)?.into_iter().rev()
            {
                decl = self.pascal_unit_declaration(&file, &lower, &r.file_path)?;
                if decl.is_some() {
                    break;
                }
            }
        }
        if let Some(decl) = decl {
            return Ok(Some(PasHead {
                ty: if CLASS_KINDS.contains(&decl.kind.as_str()) {
                    Some(decl)
                } else {
                    self.pascal_type_of_member(&decl)?
                },
                unit_file: None,
            }));
        }
        match self.pascal_unit_file_named(&lower, &r.file_path)? {
            Some(file) => Ok(Some(PasHead {
                ty: None,
                unit_file: Some(file),
            })),
            None => Ok(None),
        }
    }

    // ------------------------------------------------------------------
    // Classes, units and declarations
    // ------------------------------------------------------------------

    /// The Pascal nodes a name names, whatever their case.
    fn pascal_named(&mut self, lower: &str) -> Res<Vec<Arc<KNode>>> {
        Ok(self
            .nodes_by_lower_name(lower)?
            .iter()
            .filter(|n| n.language == "pascal")
            .cloned()
            .collect())
    }

    /// classByQualifiedName — the class (or record) an implementation
    /// header names (`TOuter::TInner`, generic parameters dropped): the
    /// one `file_path`'s unit declares, whatever its case or parameters,
    /// else one the file sees.
    fn pascal_class_by_qn(
        &mut self,
        qn: &str,
        file_path: &str,
    ) -> Res<Option<Arc<KNode>>> {
        let key = format!("{file_path}|{qn}");
        if let Some(hit) = self.pascal.class_by_qn.get(&key) {
            return Ok(hit.clone());
        }
        let exact: Vec<Arc<KNode>> = self
            .nodes_by_qualified_name(qn)?
            .iter()
            .filter(|n| {
                n.language == "pascal" && CLASS_KINDS.contains(&n.kind.as_str())
            })
            .cloned()
            .collect();
        let lower = qn.to_lowercase();
        let mut cls: Option<Arc<KNode>> = None;
        for file in self.pascal_own_unit_files(file_path)?.iter().cloned().collect::<Vec<_>>() {
            cls = pascal_declaration_of(
                exact.iter().filter(|n| n.file_path == file).cloned().collect(),
            );
            if cls.is_none() {
                let outline = self.pascal_outline(&file)?;
                cls = pascal_declaration_of(
                    outline
                        .classes
                        .iter()
                        .filter(|n| {
                            strip_generic_params(&n.qualified_name)
                                .to_lowercase()
                                == lower
                        })
                        .cloned()
                        .collect(),
                );
            }
            if cls.is_some() {
                break;
            }
        }
        if cls.is_none() {
            cls = pascal_declaration_of(exact);
            if cls.is_none() {
                cls = self.pascal_class_named(
                    qn.rsplit("::").next().unwrap_or(qn),
                    file_path,
                )?;
            }
        }
        self.pascal.class_by_qn.insert(key, cls.clone());
        Ok(cls)
    }

    /// classOfRoutine — the class a method belongs to: its qualified
    /// name's owner, declared in the method's own file.
    fn pascal_class_of_routine(
        &mut self,
        routine: &KNode,
    ) -> Res<Option<Arc<KNode>>> {
        if routine.kind != "method" {
            return Ok(None);
        }
        let owner = routine
            .qualified_name
            .rsplit_once("::")
            .map(|(o, _)| o.to_string())
            .unwrap_or_default();
        let candidates: Vec<Arc<KNode>> = if owner.is_empty() {
            Vec::new()
        } else {
            self.nodes_by_qualified_name(&owner)?
                .iter()
                .filter(|n| {
                    n.language == "pascal"
                        && CLASS_KINDS.contains(&n.kind.as_str())
                })
                .cloned()
                .collect()
        };
        Ok(pascal_declaration_of(
            candidates
                .iter()
                .filter(|n| n.file_path == routine.file_path)
                .cloned()
                .collect(),
        )
        .or_else(|| pascal_declaration_of(candidates)))
    }

    /// ancestorsOf — a class and its ancestors the project declares,
    /// nearest first.
    fn pascal_ancestors(&mut self, cls: &KNode) -> Res<Vec<Arc<KNode>>> {
        if let Some(hit) = self.pascal.ancestors.get(&cls.id) {
            return Ok(hit.as_ref().clone());
        }
        let mut order: Vec<Arc<KNode>> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut frontier = vec![Arc::new(cls.clone())];
        for _ in 0..MAX_HIERARCHY_DEPTH {
            if frontier.is_empty() {
                break;
            }
            let mut next: Vec<Arc<KNode>> = Vec::new();
            for c in frontier {
                if !seen.insert(c.id.clone()) {
                    continue;
                }
                order.push(c.clone());
                for name in self.pascal_declared_supertypes(&c)? {
                    if let Some(sup) =
                        self.pascal_class_named(&name, &c.file_path)?
                    {
                        next.push(sup);
                    }
                }
            }
            frontier = next;
        }
        self.pascal
            .ancestors
            .insert(cls.id.clone(), Rc::new(order.clone()));
        Ok(order)
    }

    /// declaredSupertypes — the supertypes a Pascal class's own
    /// declaration names (`TFoo = class(TBar)`), memoized.
    fn pascal_declared_supertypes(&mut self, cls: &KNode) -> Res<Vec<String>> {
        if let Some(hit) = self.pascal.supers.get(&cls.id) {
            return Ok(hit.as_ref().clone());
        }
        let mut supers = Vec::new();
        if let Some(source) = self.read_file(&cls.file_path) {
            let lines: &[String] = &source[..];
            let from = (cls.start_line - 1).max(0) as usize;
            let head = lines
                .get(from..(from + 3).min(lines.len()))
                .map(|s| s.join(" "))
                .unwrap_or_default();
            if let Some(m) = re!(
                r"(?i)=\s*class\s*(?:sealed|abstract)?\s*\(([^)]*)\)"
            )
            .captures(&head)
            {
                supers = re!(r"[A-Za-z_][\w.:\\]*")
                    .find_iter(&m[1])
                    .map(|w| {
                        w.as_str()
                            .rsplit([':', '.', '\\'])
                            .next()
                            .unwrap_or(w.as_str())
                            .to_string()
                    })
                    .filter(|w| {
                        !matches!(
                            w.to_lowercase().as_str(),
                            "metaclass" | "object"
                        )
                    })
                    .collect();
            }
        }
        self.pascal
            .supers
            .insert(cls.id.clone(), Rc::new(supers.clone()));
        Ok(supers)
    }

    /// memberInHierarchy — the member a name reaches in a class or,
    /// failing that, in its ancestors, nearest first; a parameterless
    /// overload before the others. None when no class of the hierarchy
    /// declares it.
    fn pascal_member_in_hierarchy(
        &mut self,
        cls: &KNode,
        lower: &str,
    ) -> Res<Option<Arc<KNode>>> {
        let key = format!("{}|{}", cls.id, lower);
        if let Some(hit) = self.pascal.hierarchy.get(&key) {
            return Ok(hit.clone());
        }
        let mut found: Option<Arc<KNode>> = None;
        let candidates: Vec<Arc<KNode>> = self
            .pascal_named(lower)?
            .into_iter()
            .filter(|n| MEMBER_KINDS.contains(&n.kind.as_str()))
            .collect();
        if !candidates.is_empty() {
            for c in self.pascal_ancestors(cls)? {
                let owner = c.qualified_name.to_lowercase();
                let members: Vec<Arc<KNode>> = candidates
                    .iter()
                    .filter(|n| {
                        n.file_path == c.file_path
                            && pascal_owner_of(n) == owner
                    })
                    .cloned()
                    .collect();
                if !members.is_empty() {
                    found = Some(
                        members
                            .iter()
                            .find(|n| !pascal_needs_arguments(n))
                            .cloned()
                            .unwrap_or_else(|| members[0].clone()),
                    );
                    break;
                }
            }
        }
        self.pascal.hierarchy.insert(key, found.clone());
        Ok(found)
    }

    /// unitDeclaration — a unit's top-level declaration of a name (its
    /// enum values included), as code in `from_file` sees it: another
    /// unit's member that its parse lost (see PasOutline::routine_column)
    /// is not one, while in its own unit it is most likely the member the
    /// code calls.
    fn pascal_unit_declaration(
        &mut self,
        file_path: &str,
        lower: &str,
        from_file: &str,
    ) -> Res<Option<Arc<KNode>>> {
        let routine_column = if from_file != file_path {
            self.pascal_outline(file_path)?.routine_column
        } else {
            i64::MAX
        };
        let decls: Vec<Arc<KNode>> = self
            .pascal_named(lower)?
            .into_iter()
            .filter(|n| {
                n.file_path == file_path
                    && UNIT_KINDS.contains(&n.kind.as_str())
                    && (n.kind == "enum_member"
                        || !n.qualified_name.contains("::"))
                    && !(from_file != file_path
                        && n.kind == "function"
                        && n.start_column > routine_column)
            })
            .collect();
        // A routine declared in the interface and defined in the
        // implementation is one name; of overloads, one called without
        // arguments first.
        Ok(decls
            .iter()
            .find(|n| !pascal_is_routine(n))
            .cloned()
            .or_else(|| {
                decls
                    .iter()
                    .find(|n| !pascal_needs_arguments(n))
                    .cloned()
            })
            .or_else(|| decls.first().cloned()))
    }

    /// usedUnitFiles — the files of the units a file uses, in the order
    /// its `uses` clauses name them. The unit's `uses` clauses, then those
    /// of the files it includes: mORMot's `.inc` files open the unit's
    /// implementation with one.
    fn pascal_used_unit_files(&mut self, from_file: &str) -> Res<Vec<String>> {
        if let Some(hit) = self.pascal.used_files.get(from_file) {
            return Ok(hit.clone());
        }
        let unit = self.pascal_unit_files_of(from_file)?;
        let mut files: Vec<String> = Vec::new();
        for own in unit.clone() {
            let imports = self.pascal_outline(&own)?.imports.clone();
            for imp in imports {
                if let Some(file) =
                    self.pascal_unit_file_named(&imp.name.to_lowercase(), &own)?
                {
                    if !unit.contains(&file) && !files.contains(&file) {
                        files.push(file);
                    }
                }
            }
        }
        self.pascal
            .used_files
            .insert(from_file.to_string(), files.clone());
        Ok(files)
    }

    /// ownUnitDeclaration — the unit's own declaration of a name, seen
    /// from one of its files (see pascal_own_unit_files).
    fn pascal_own_unit_declaration(
        &mut self,
        from_file: &str,
        lower: &str,
    ) -> Res<Option<Arc<KNode>>> {
        for file in self.pascal_own_unit_files(from_file)?.iter().cloned().collect::<Vec<_>>() {
            if let Some(decl) = self.pascal_unit_declaration(&file, lower, &file)? {
                return Ok(Some(decl));
            }
        }
        Ok(None)
    }

    /// ownUnitFiles — the files whose declarations are a file's unit's
    /// own, the file first: the unit is its own file and the include
    /// files it pulls in, so code in `mormot.core.os.pas` sees a variable
    /// `mormot.core.os.posix.inc` declares, and that file's code sees the
    /// unit's (see pascal_unit_files_of).
    fn pascal_own_unit_files(&mut self, file_path: &str) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.pascal.own_files.get(file_path) {
            return Ok(hit.clone());
        }
        let unit = self.pascal_unit_files_of(file_path)?;
        let own: Vec<String> = if unit.first().map(String::as_str) == Some(file_path) {
            unit
        } else {
            let mut v = vec![file_path.to_string()];
            v.extend(unit.into_iter().filter(|f| f != file_path));
            v
        };
        let rc = Rc::new(own);
        self.pascal
            .own_files
            .insert(file_path.to_string(), rc.clone());
        Ok(rc)
    }

    /// unitFilesOf — the files one unit is made of: its own file, then
    /// the include files it pulls in (`{$I name.inc}`, `{$INCLUDE name}`),
    /// nested ones too. For an include file, the unit that includes it,
    /// nearest it when several do.
    fn pascal_unit_files_of(&mut self, file_path: &str) -> Res<Vec<String>> {
        if let Some(hit) = self.pascal.unit_file_sets.get(file_path) {
            return Ok(hit.as_ref().clone());
        }
        let mut root = file_path.to_string();
        for _ in 0..3 {
            if !re!(r"(?i)\.inc$").is_match(&root) {
                break;
            }
            match self.pascal_includer_of(&root)? {
                Some(up) if up != file_path => root = up,
                _ => break,
            }
        }
        let mut files = vec![root];
        let mut i = 0usize;
        while i < files.len() && files.len() < 32 {
            let incs = self.pascal_includes_of(&files[i].clone())?;
            for inc in incs.iter() {
                if !files.contains(inc) {
                    files.push(inc.clone());
                }
            }
            i += 1;
        }
        if !files.iter().any(|f| f == file_path) {
            files.push(file_path.to_string());
        }
        let rc = Rc::new(files);
        self.pascal
            .unit_file_sets
            .insert(file_path.to_string(), rc.clone());
        Ok(rc.as_ref().clone())
    }

    /// includesOf — the include files a Pascal file pulls in
    /// (`{$I name.inc}`, `{$INCLUDE name}`), each the one of that name
    /// nearest it.
    fn pascal_includes_of(&mut self, file_path: &str) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.pascal.includes.get(file_path) {
            return Ok(hit.clone());
        }
        let mut includes: Vec<String> = Vec::new();
        if re!(PASCAL_SOURCE).is_match(file_path) {
            if let Some(source) = self.read_file(file_path) {
                for text in source.iter() {
                    if !text.contains("{$")
                        || !re!(r"(?i)\{\$i(?:nclude)?\s").is_match(text)
                    {
                        continue;
                    }
                    let project = self.pascal_project_files()?;
                    for m in
                        re!(r#"(?i)\{\$(?:I|INCLUDE)\s+['"]?([^'"}\s*]+)"#)
                            .captures_iter(text)
                    {
                        let name = m[1].replace('\\', "/");
                        let mut name = name
                            .rsplit('/')
                            .next()
                            .unwrap_or(&name)
                            .to_lowercase();
                        if !name.contains('.') {
                            name += ".inc";
                        }
                        let candidates = project
                            .by_name
                            .get(&name)
                            .cloned()
                            .unwrap_or_default();
                        if let Some(inc) = pascal_nearest(candidates, file_path) {
                            if inc != file_path && !includes.contains(&inc) {
                                includes.push(inc);
                            }
                        }
                    }
                }
            }
        }
        let rc = Rc::new(includes);
        self.pascal
            .includes
            .insert(file_path.to_string(), rc.clone());
        Ok(rc)
    }

    /// includerOf — the Pascal file that includes an include file: of the
    /// forty nearest it, the nearest whose text does. The files of its
    /// directory first, those named most like it first of them
    /// (`mormot.core.os.pas` for `mormot.core.os.posix.inc`): how many
    /// characters of its path a file shares, the directory counting
    /// first.
    fn pascal_includer_of(&mut self, inc_file: &str) -> Res<Option<String>> {
        if let Some(hit) = self.pascal.includers.get(inc_file) {
            return Ok(hit.clone());
        }
        let pascal = self.pascal_project_files()?.pascal.clone();
        let ib = inc_file.as_bytes();
        let shared = |f: &str| -> i64 {
            let fb = f.as_bytes();
            let mut n = 0usize;
            while n < fb.len() && n < ib.len() && fb[n] == ib[n] {
                n += 1;
            }
            let slash = f[..n.min(f.len())].rfind('/').map(|i| i as i64).unwrap_or(-1);
            slash * 1000 + n as i64
        };
        let mut candidates: Vec<(i64, String)> = pascal
            .iter()
            .filter(|f| f.as_str() != inc_file)
            .map(|f| (shared(f), f.clone()))
            .collect();
        candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
        candidates.truncate(40);
        let mut includer: Option<String> = None;
        for (_, f) in candidates {
            if self.pascal_includes_of(&f)?.iter().any(|i| i == inc_file) {
                includer = Some(f);
                break;
            }
        }
        self.pascal
            .includers
            .insert(inc_file.to_string(), includer.clone());
        Ok(includer)
    }

    /// projectFiles — the project's files by lowercased name, and its
    /// Pascal sources.
    fn pascal_project_files(&mut self) -> Res<Rc<PasProjectFiles>> {
        if let Some(hit) = &self.pascal.project_files {
            return Ok(hit.clone());
        }
        let files: Vec<String> = match self.sorted_files() {
            Some(f) => f.as_ref().clone(),
            None => {
                let mut f: Vec<String> =
                    self.table()?.files.iter().cloned().collect();
                f.sort();
                f
            }
        };
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        let mut pascal: Vec<String> = Vec::new();
        for file in &files {
            let base = file.rsplit('/').next().unwrap_or(file).to_lowercase();
            by_name.entry(base).or_default().push(file.clone());
            if re!(PASCAL_SOURCE).is_match(file) {
                pascal.push(file.clone());
            }
        }
        let rc = Rc::new(PasProjectFiles { by_name, pascal });
        self.pascal.project_files = Some(rc.clone());
        Ok(rc)
    }

    /// unitFileNamed — the file of the unit a name names, nearest to
    /// `from_file` when several units share it.
    fn pascal_unit_file_named(
        &mut self,
        lower: &str,
        from_file: &str,
    ) -> Res<Option<String>> {
        let key = format!("{from_file}|{lower}");
        if let Some(hit) = self.pascal.unit_files.get(&key) {
            return Ok(hit.clone());
        }
        if self.pascal.units_by_name.is_none() {
            let mut units: HashMap<String, Vec<String>> = HashMap::new();
            let mut headed: HashSet<String> = HashSet::new();
            for file in self.pascal_project_files()?.pascal.clone() {
                for n in self.nodes_in_file(&file)?.iter() {
                    if n.kind == "module" && n.language == "pascal" {
                        units
                            .entry(n.name.to_lowercase())
                            .or_default()
                            .push(n.file_path.clone());
                        headed.insert(n.file_path.clone());
                    }
                }
            }
            // A unit's file is named after it, which Delphi enforces: one
            // whose `unit` header the parser lost to an error is the unit
            // its file names.
            for file in self.pascal_project_files()?.pascal.clone() {
                if headed.contains(&file)
                    || !file.to_lowercase().ends_with(".pas")
                {
                    continue;
                }
                let base = file.rsplit('/').next().unwrap_or(&file);
                if let Some(stem) = base.get(..base.len().saturating_sub(4)) {
                    units
                        .entry(stem.to_lowercase())
                        .or_default()
                        .push(file.clone());
                }
            }
            self.pascal.units_by_name = Some(Rc::new(units));
        }
        let units = self.pascal.units_by_name.as_ref().unwrap().clone();
        let file = pascal_nearest(
            units.get(lower).cloned().unwrap_or_default(),
            from_file,
        );
        self.pascal.unit_files.insert(key, file.clone());
        Ok(file)
    }

    /// What scope lookups need of a file's nodes, read once: its classes,
    /// its `uses` in order, its routine indentation limit.
    fn pascal_outline(&mut self, file_path: &str) -> Res<Rc<PasOutline>> {
        if let Some(hit) = self.pascal.outlines.get(file_path) {
            return Ok(hit.clone());
        }
        let nodes = self.nodes_in_file(file_path)?;
        let mut routine_column = i64::MAX;
        if !nodes.iter().any(|n| n.kind == "module") {
            for n in nodes.iter() {
                if n.kind == "function" && !n.qualified_name.contains("::") {
                    routine_column = routine_column.min(n.start_column);
                }
            }
        }
        let classes: Vec<Arc<KNode>> = nodes
            .iter()
            .filter(|n| CLASS_KINDS.contains(&n.kind.as_str()))
            .cloned()
            .collect();
        let mut imports: Vec<Arc<KNode>> = nodes
            .iter()
            .filter(|n| n.kind == "import" && n.language == "pascal")
            .cloned()
            .collect();
        imports.sort_by_key(|n| (n.start_line, n.start_column));
        let rc = Rc::new(PasOutline {
            classes,
            imports,
            routine_column,
        });
        self.pascal
            .outlines
            .insert(file_path.to_string(), rc.clone());
        Ok(rc)
    }

    /// classNamed — a class (or record or interface) a type name names,
    /// seen from `from_file`; a type alias names the class it stands for
    /// (`TVTAncestor = TVTAncestorVcl;`).
    fn pascal_class_named(
        &mut self,
        type_name: &str,
        from_file: &str,
    ) -> Res<Option<Arc<KNode>>> {
        self.pascal_class_named_depth(type_name, from_file, 0)
    }

    fn pascal_class_named_depth(
        &mut self,
        type_name: &str,
        from_file: &str,
        depth: usize,
    ) -> Res<Option<Arc<KNode>>> {
        let simple = type_name
            .rsplit('.')
            .next()
            .unwrap_or(type_name)
            .to_lowercase();
        let key = format!("{from_file}|{simple}");
        if let Some(hit) = self.pascal.class_by_name.get(&key) {
            return Ok(hit.clone());
        }
        let named = self.pascal_named(&simple)?;
        let mut cls =
            pascal_declaration_of(self.pascal_visible_named(&named, CLASS_KINDS, from_file)?);
        if cls.is_none() && depth < 4 {
            for alias in
                self.pascal_visible_named(&named, &["type_alias"], from_file)?
            {
                let line = self
                    .read_file(&alias.file_path)
                    .and_then(|s| {
                        s.get((alias.start_line - 1).max(0) as usize).cloned()
                    })
                    .unwrap_or_default();
                let stripped = re!(r"//.*$|\{[^}]*\}")
                    .replace_all(&line, "")
                    .into_owned();
                if let Some(m) =
                    re!(r"=\s*([A-Za-z_][\w.]*)\s*;").captures(&stripped)
                {
                    if let Some(c) = self.pascal_class_named_depth(
                        &m[1],
                        &alias.file_path,
                        depth + 1,
                    )? {
                        cls = Some(c);
                        break;
                    }
                }
            }
        }
        self.pascal.class_by_name.insert(key, cls.clone());
        Ok(cls)
    }

    /// The candidates of `kinds` Delphi sees from `from_file`: in the
    /// unit's own files, then in the unit used last, which declares the
    /// name Delphi sees; else the one unit that does.
    fn pascal_visible_named(
        &mut self,
        named: &[Arc<KNode>],
        kinds: &[&str],
        from_file: &str,
    ) -> Res<Vec<Arc<KNode>>> {
        let candidates: Vec<Arc<KNode>> = named
            .iter()
            .filter(|n| kinds.contains(&n.kind.as_str()))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(candidates);
        }
        for file in self.pascal_own_unit_files(from_file)?.iter() {
            let own: Vec<Arc<KNode>> = candidates
                .iter()
                .filter(|n| n.file_path == *file)
                .cloned()
                .collect();
            if !own.is_empty() {
                return Ok(own);
            }
        }
        for file in self.pascal_used_unit_files(from_file)?.into_iter().rev() {
            let there: Vec<Arc<KNode>> = candidates
                .iter()
                .filter(|n| n.file_path == file)
                .cloned()
                .collect();
            if !there.is_empty() {
                return Ok(there);
            }
        }
        if candidates
            .iter()
            .all(|n| n.file_path == candidates[0].file_path)
        {
            return Ok(candidates);
        }
        Ok(Vec::new())
    }

    // ------------------------------------------------------------------
    // Declared types
    // ------------------------------------------------------------------

    /// typeOfMember — the class a member's value has: a field's or
    /// variable's declared type, a property's, a function's result.
    fn pascal_type_of_member(
        &mut self,
        member: &KNode,
    ) -> Res<Option<Arc<KNode>>> {
        if let Some(hit) = self.pascal.member_types.get(&member.id) {
            return Ok(hit.clone());
        }
        let type_name: Option<String> = if matches!(
            member.kind.as_str(),
            "method" | "function"
        ) {
            member.return_type.clone().or_else(|| {
                re!(r":\s*([A-Za-z_][\w.]*)\s*;?\s*$")
                    .captures(member.signature.as_deref().unwrap_or(""))
                    .map(|m| m[1].to_string())
            })
        } else {
            let text = self.pascal_declared_type_text(member)?;
            re!(r"^([A-Za-z_][\w.]*)")
                .captures(&text)
                .map(|m| m[1].to_string())
        };
        let ty = match type_name {
            Some(t) => self.pascal_class_named(&t, &member.file_path)?,
            None => None,
        };
        self.pascal.member_types.insert(member.id.clone(), ty.clone());
        Ok(ty)
    }

    /// declaredTypeText — the text of the type a field, variable,
    /// constant or property declares, from its declaration on: `TFoo;
    /// // the foo` for `FFoo: TFoo; // the foo`. A name in a comma list
    /// (`FOnLoad,` above `FOnSave: TSaveEvent;`) takes the type the list
    /// ends with, a few lines down. Empty when none is written, as for a
    /// published `property OnClick;`.
    fn pascal_declared_type_text(&mut self, node: &KNode) -> Res<String> {
        let Some(source) = self.read_file(&node.file_path) else {
            return Ok(String::new());
        };
        let name = regex::escape(&node.name);
        // `:` that does not start `:=` — Rust regex has no lookahead, so
        // the colon is found first and the next byte checked by hand.
        let rx = if node.kind == "property" {
            Self::cached_regex(&format!(
                r"(?i)\bproperty\s+{name}\s*(?:\[[^\]]*\])?\s*:"
            ))?
        } else {
            Self::cached_regex(&format!(
                r"(?i)\b{name}\b(?:\s*,\s*[A-Za-z_]\w*)*\s*,?\s*:"
            ))?
        };
        let strip = re!(r"//.*$|\{[^}]*\}|\(\*.*?\*\)");
        let mut text = String::new();
        let start = (node.start_line - 1).max(0) as usize;
        for i in start..(start + 6).min(source.len()) {
            text.push(' ');
            text.push_str(&strip.replace_all(&source[i], " "));
            if let Some(m) = rx.find(&text) {
                let after = &text[m.end()..];
                if !after.starts_with('=') {
                    return Ok(after.trim().to_string());
                }
            }
            // A list goes on only after a comma.
            if node.kind == "property" || !re!(r",\s*$").is_match(&text) {
                break;
            }
        }
        Ok(String::new())
    }

    /// isProceduralData — whether data holds a routine, by its declared
    /// type: one written in place (`FProc: procedure of object`,
    /// `F: reference to function: Integer`), a project type declared as
    /// one (`TOnLog = procedure(...) of object;`), or a type the project
    /// doesn't declare that the RTL's names say is one (`TNotifyEvent`,
    /// `TWndMethod`, `TProc<T>`).
    fn pascal_is_procedural_data(&mut self, node: &KNode) -> Res<bool> {
        if let Some(hit) = self.pascal.procedural.get(&node.id) {
            return Ok(*hit);
        }
        let text = self.pascal_declared_type_text(node)?;
        let procedural = self.pascal_is_procedural_type_text(&text, 0)?;
        self.pascal.procedural.insert(node.id.clone(), procedural);
        Ok(procedural)
    }

    /// isProceduralTypeText — whether a written type is a routine's:
    /// written out (`reference to function: Integer`), or named, with the
    /// name's declaration saying so, through an alias to another such
    /// name (`TFormatter = TFunc<Exception, string>;`). A name the
    /// project doesn't declare is a routine type when the RTL's names
    /// say so: `TProc`, `TFunc<T>`, and the names Delphi gives them
    /// (`TNotifyEvent`, `TWndMethod`).
    fn pascal_is_procedural_type_text(
        &mut self,
        text: &str,
        depth: usize,
    ) -> Res<bool> {
        if re!(PROCEDURAL_TYPE).is_match(text) {
            return Ok(true);
        }
        let type_name = re!(r"(?i)^(?:type\s+)?([A-Za-z_][\w.]*)")
            .captures(text)
            .map(|m| {
                m[1].rsplit('.').next().unwrap_or(&m[1]).to_lowercase()
            });
        let Some(type_name) = type_name else {
            return Ok(false);
        };
        if depth > 3 {
            return Ok(false);
        }
        let types = self.pascal_types_named(&type_name)?;
        if types.is_empty() {
            return Ok(RTL_PROCEDURAL_TYPES.contains(type_name.as_str())
                || re!(r"(?i)^t\w*(?:event|proc|method|callback)$")
                    .is_match(&type_name));
        }
        for t in types {
            if t.kind == "type_alias" {
                let aliased = self.pascal_aliased_type_text(&t)?;
                if self.pascal_is_procedural_type_text(&aliased, depth + 1)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// aliasedTypeText — what a type declaration says after its `=`,
    /// which may start on the next line: `procedure(...) of object` for
    /// `TOnHelper =` above it.
    fn pascal_aliased_type_text(&mut self, alias: &KNode) -> Res<String> {
        let Some(source) = self.read_file(&alias.file_path) else {
            return Ok(String::new());
        };
        let strip = re!(r"//.*$|\{[^}]*\}|\(\*.*?\*\)");
        let mut text = String::new();
        let start = (alias.start_line - 1).max(0) as usize;
        for i in start..(start + 3).min(source.len()) {
            text.push(' ');
            text.push_str(&strip.replace_all(&source[i], " "));
            if let Some(eq) = text.find('=') {
                let rest = text[eq + 1..].trim();
                if !rest.is_empty() {
                    return Ok(rest.to_string());
                }
            }
        }
        Ok(String::new())
    }

    /// typesNamed — the project's Pascal types a name names, generic
    /// ones by the name before their parameters: `TSetParams<T: class>`
    /// for `TSetParams`.
    fn pascal_types_named(&mut self, lower: &str) -> Res<Vec<Arc<KNode>>> {
        let generic = self.pascal_generic_types()?;
        let mut out: Vec<Arc<KNode>> = self
            .pascal_named(lower)?
            .into_iter()
            .filter(|t| {
                t.kind == "type_alias" || CLASS_KINDS.contains(&t.kind.as_str())
            })
            .collect();
        out.extend(generic.get(lower).cloned().unwrap_or_default());
        Ok(out)
    }

    /// The generic Pascal types by the name before their parameters.
    fn pascal_generic_types(
        &mut self,
    ) -> Res<Rc<HashMap<String, Vec<Arc<KNode>>>>> {
        if let Some(hit) = &self.pascal.generic_types {
            return Ok(hit.clone());
        }
        let mut generic: HashMap<String, Vec<Arc<KNode>>> = HashMap::new();
        for file in self.pascal_project_files()?.pascal.clone() {
            for n in self.nodes_in_file(&file)?.iter() {
                if n.language != "pascal" {
                    continue;
                }
                let Some(open) = n.name.find('<') else {
                    continue;
                };
                if open == 0
                    || !(n.kind == "type_alias"
                        || CLASS_KINDS.contains(&n.kind.as_str()))
                {
                    continue;
                }
                let base = n.name[..open].trim().to_lowercase();
                generic.entry(base).or_default().push(n.clone());
            }
        }
        let rc = Rc::new(generic);
        self.pascal.generic_types = Some(rc.clone());
        Ok(rc)
    }

    /// yieldsValue — whether a routine gives a value — a function or a
    /// constructor — rather than a procedure.
    fn pascal_yields_value(&mut self, routine: &KNode) -> Res<bool> {
        if routine.return_type.is_some() {
            return Ok(true);
        }
        Ok(matches!(
            self.pascal_routine_keyword(routine)?.as_deref(),
            Some("function") | Some("constructor")
        ))
    }

    /// routineKeyword — the keyword that declares a routine
    /// (`function`, `procedure`, `constructor`, …), lowercased.
    fn pascal_routine_keyword(
        &mut self,
        routine: &KNode,
    ) -> Res<Option<String>> {
        if let Some(hit) = self.pascal.keywords.get(&routine.id) {
            return Ok(hit.clone());
        }
        let keyword = self
            .read_file(&routine.file_path)
            .and_then(|s| {
                s.get((routine.start_line - 1).max(0) as usize).cloned()
            })
            .and_then(|line| {
                re!(r"(?i)\b(function|procedure|constructor|destructor|operator)\s")
                    .captures(&line)
                    .map(|m| m[1].to_lowercase())
            });
        self.pascal
            .keywords
            .insert(routine.id.clone(), keyword.clone());
        Ok(keyword)
    }
}

/// `{...result, targetNodeId, confidence, resolvedBy}` — the gate's
/// retarget keeps the rest of the verdict.
fn retarget(mut out: ResolveOutcome, decl: &PasFound) -> ResolveOutcome {
    out.target_node_id = Some(decl.node.id.clone());
    out.confidence = Some(decl.confidence);
    out.resolved_by = Some(decl.resolved_by.to_string());
    out
}

fn pascal_is_routine(node: &KNode) -> bool {
    matches!(node.kind.as_str(), "method" | "function")
}

/// The qualified name of what declares a node (`TFoo::Bar` → `tfoo`),
/// lowercased.
fn pascal_owner_of(node: &KNode) -> String {
    node.qualified_name
        .rsplit_once("::")
        .map(|(o, _)| o.to_lowercase())
        .unwrap_or_default()
}

/// The class declaration among same-named nodes, not a forward
/// declaration (`TFoo = class;`, which names no members or ancestors):
/// the longest one.
fn pascal_declaration_of(classes: Vec<Arc<KNode>>) -> Option<Arc<KNode>> {
    classes
        .into_iter()
        .max_by_key(|c| c.end_line - c.start_line)
}

/// conditionDepthAfter — the depth of conditional blocks after a line's
/// directives (`{$IFDEF X}` opens one, `{$ENDIF}` closes it).
fn pascal_condition_depth_after(text: &str, mut depth: i64) -> i64 {
    for m in re!(r"(?i)\{\$(ifdef|ifndef|ifopt|ifend|endif|if)\b")
        .captures_iter(text)
    {
        let directive = m[1].to_lowercase();
        depth = if directive == "endif" || directive == "ifend" {
            (depth - 1).max(0)
        } else {
            depth + 1
        };
    }
    depth
}

/// The file whose path shares the longest directory prefix with `from`.
fn pascal_nearest(files: Vec<String>, from: &str) -> Option<String> {
    if files.len() <= 1 {
        return files.into_iter().next();
    }
    let dirs: Vec<&str> = from.split('/').collect();
    let mut best: Option<String> = None;
    let mut best_score = -1i64;
    for f in files {
        let parts: Vec<&str> = f.split('/').collect();
        let mut score = 0usize;
        while score < parts.len() && score < dirs.len()
            && parts[score] == dirs[score]
        {
            score += 1;
        }
        if score as i64 > best_score {
            best = Some(f);
            best_score = score as i64;
        }
    }
    best
}

/// `TList<T>` → `TList` — qualified names carry no generic parameters.
fn strip_generic_params(qn: &str) -> String {
    re!(r"<[^>]*>").replace_all(qn, "").into_owned()
}

/// Whether a routine has a parameter without a default value, which a
/// call must pass.
fn pascal_needs_arguments(routine: &KNode) -> bool {
    let Some(signature) = routine.signature.as_deref() else {
        return false;
    };
    let signature = signature.trim();
    if !signature.starts_with('(') {
        return false;
    }
    let mut depth = 0i32;
    let mut end = signature.len();
    for (i, ch) in signature.char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                end = i;
                break;
            }
        }
    }
    signature[1..end]
        .split(';')
        .any(|param| !param.trim().is_empty() && !param.contains('='))
}
