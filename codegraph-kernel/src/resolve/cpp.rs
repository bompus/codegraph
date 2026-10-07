//! C/C++ call-less shapes (#1838, #1839): cpp-macro-visibility.ts and
//! cpp-constructor.ts.
//!
//! `TRACE_POINT(1)` parses as a call, so extraction records a `calls` ref
//! named `TRACE_POINT`. When the translation unit defines the function-like
//! macro at that point (the file itself or an in-repo include), the "call" is
//! a macro expansion and must not bind to a same-spelled function elsewhere.
//!
//! `T obj;` / `T obj(args);` / `T obj{args};` carry no call node, so
//! extraction records a `calls` ref shaped `ns::T::T/<arity>`; it resolves
//! only to the constructor of the lexically nearest `T` whose parameter count
//! admits the arguments, and never falls through to the name strategies.

use super::*;

/// Three-valued: `None` = depends on an unknown build flag.
type Truth = Option<bool>;

fn and(a: Truth, b: Truth) -> Truth {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

fn or(a: Truth, b: Truth) -> Truth {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

fn not(a: Truth) -> Truth {
    a.map(|v| !v)
}

enum FileEvent {
    Define { define: bool, name: String, line: i64, function_like: bool, value: String, wraps_itself: bool },
    Include { quote: char, spec: String, line: i64 },
    Branch { op: String, expression: String, guard: bool, line: i64 },
    Once { line: i64 },
}

impl FileEvent {
    fn line(&self) -> i64 {
        match self {
            FileEvent::Define { line, .. } | FileEvent::Include { line, .. } | FileEvent::Branch { line, .. } | FileEvent::Once { line } => *line,
        }
    }
}

#[derive(Clone, Copy)]
struct Event {
    line: i64,
    defined: Truth,
}

type Timeline = HashMap<String, Vec<Event>>;

/// A root file's macro timeline; from `cutoff` on, nothing is known.
struct RootTimeline {
    events: Timeline,
    cutoff: Option<i64>,
}
type TypeAliases = HashMap<String,(HashSet<String>,bool)>;

const ROOT_TIMELINE_CAP: usize = 32;
/// Directive events one translation-unit walk may evaluate (#2127).
const WALK_EVENT_BUDGET: usize = 1_000_000;

/// Directive summaries are cached per file but evaluated in translation-unit
/// order on every inclusion: an included file can change its flags.
#[derive(Default)]
pub(super) struct MacroCache {
    summaries: HashMap<String, Rc<Vec<FileEvent>>>,
    includes: HashMap<String, Option<String>>,
    /// Indexed files by basename, for `#include "dir/name.h"` no include root explains.
    by_basename: Option<HashMap<String, Vec<String>>>,
    /// Per root file (oldest first): macro name → define/undef events in root-file line order.
    roots: VecDeque<(String, Rc<RootTimeline>)>,
    type_aliases: VecDeque<(String,Rc<TypeAliases>)>,
}

/// CPP_DEFINE_SIGNATURE (types.ts): the constant extraction mints from a
/// function-like `preproc_function_def`. A macro is a value, never a callee.
pub(super) fn is_define(n: &KNode) -> bool {
    n.kind == "constant" && re!(r"^\s*#\s*define\b").is_match(n.signature.as_deref().unwrap_or(""))
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[derive(Clone, PartialEq)]
struct Definition {
    defined: Truth,
    /// The replacement text, read when a condition names the macro: an
    /// alias (`#define FLAG BACKING`) takes `BACKING`'s value at that point.
    value: Option<Rc<str>>,
    is_macro: Truth,
}

/// Alias chains deeper than this (or cyclic) are an unknown build flag.
const ALIAS_DEPTH: u32 = 8;

/// The arms of one `#if` block seen so far, and for each name the arms that
/// `#define`/`#undef` it: (last arm, arms counted, agreed macro-ness).
#[derive(Default)]
struct Arms {
    arm: usize,
    in_else: bool,
    aliases: TypeAliases,
    defs: HashMap<String, (usize, usize, Truth)>,
}

/// One finished visit of a file: the truth it was entered under, `version`
/// before and after, and the include cycles the recursion stack cut in it.
struct Visit {
    inherited: Truth,
    start: u64,
    end: u64,
    cuts: Vec<String>,
}

/// walkTranslationUnit's state for one root file.
///
/// A guard the walk cannot decide (a header reached under an unknown `#if`)
/// re-enters its header on every inclusion path, which is exponential in the
/// include graph's depth (#2127). Every change a later directive could observe
/// (definitions, `#pragma once`, the macro-name set, type aliases) bumps
/// `version`. A re-entry with the same inherited truth, while `version` still
/// equals that of an earlier visit which itself changed nothing, starts from
/// the same state, so it would take the same branches and change nothing
/// again; the events it would push repeat each name's current state, already
/// its last event. Include cycles that visit cut must still be cut. A hard
/// budget bounds whatever is left: past it, nothing is known and nothing is
/// suppressed.
#[derive(Default)]
struct TuWalk {
    timeline: Timeline,
    definitions: HashMap<String, Definition>,
    scanning: HashSet<String>,
    macro_names: HashSet<String>,
    once: HashMap<String, Truth>,
    type_aliases: Option<TypeAliases>,
    stop_at: Option<(String,i64)>,
    stopped: bool,
    version: u64,
    cuts: Vec<String>,
    visits: HashMap<String, Visit>,
    spent: usize,
    cutoff: Option<i64>,
}

impl TuWalk {
    fn condition(&self, expression: &str) -> Truth {
        self.condition_at(expression, 0)
    }

    fn condition_at(&self, expression: &str, depth: u32) -> Truth {
        let text = expression.trim();
        if re!(r"(?i)^(?:0x[\da-f]+|\d+)[ul]*$").is_match(text) {
            let digits = text.trim_end_matches(['u', 'U', 'l', 'L']);
            let digits = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")).unwrap_or(digits);
            return Some(digits.bytes().any(|b| b != b'0'));
        }
        if let Some(c) = re!(r"^(!)?\s*defined\s*(?:\(\s*(\w+)\s*\)|(\w+))$").captures(text) {
            let name = c.get(2).or_else(|| c.get(3)).map_or("", |m| m.as_str());
            let known = self.definitions.get(name).and_then(|d| d.defined);
            return if c.get(1).is_some() { not(known) } else { known };
        }
        if re!(r"^\w+$").is_match(text) {
            if depth >= ALIAS_DEPTH {
                return None;
            }
            let definition = self.definitions.get(text)?;
            // A name `#undef`ed for certain reads as 0 in `#if`, like any
            // undefined identifier; one the walk has not seen stays unknown.
            if definition.defined == Some(false) {
                return Some(false);
            }
            let value = definition.value.clone()?;
            return self.condition_at(&value, depth + 1);
        }
        None
    }
}

impl KernelResolver {
    /// isVisibleCppMacro: is this C/C++ `calls` ref a macro expansion rather
    /// than a call? True when the index knows the name as a function-like
    /// macro and either nothing but macros bears the name (a fuzzy `SWAP` →
    /// `swap` must not invent a callee) or the macro is definitely visible at
    /// the call site.
    pub(super) fn is_visible_cpp_macro(&mut self, r: &ResolveRefIn) -> Res<bool> {
        if r.language != "c" && r.language != "cpp" {
            return Ok(false);
        }
        if r.reference_kind != "calls" || !re!(r"^\w+$").is_match(&r.reference_name) {
            return Ok(false);
        }
        let same_name = self.nodes_by_name(&r.reference_name)?;
        if !same_name.iter().any(|n| is_define(n)) {
            return Ok(false);
        }
        if same_name.iter().all(|n| is_define(n)) {
            return Ok(true);
        }
        // `(TRACE_POINT)(1)`: a function-like macro expands only when its
        // name is directly followed by `(`, so a parenthesized callee is a call.
        let parenthesized = self
            .read_file(&r.file_path)
            .and_then(|lines| lines.get((r.line - 1).max(0) as usize).map(|l| js_slice(l, r.column.max(0) as usize).starts_with('(')));
        if parenthesized == Some(true) {
            return Ok(false);
        }
        let root_key = format!("{}\0{}", r.language, r.file_path);
        let hit = self.cpp_macros.roots.iter().find(|(k, _)| *k == root_key).map(|(_, t)| t.clone());
        let timeline = match hit {
            Some(t) => t,
            None => {
                let t = Rc::new(self.walk_translation_unit(&r.file_path, &r.language));
                if self.cpp_macros.roots.len() >= ROOT_TIMELINE_CAP {
                    self.cpp_macros.roots.pop_front();
                }
                self.cpp_macros.roots.push_back((root_key, t.clone()));
                t
            }
        };
        if timeline.cutoff.is_some_and(|cutoff| r.line >= cutoff) {
            return Ok(false);
        }
        let last = timeline
            .events
            .get(&r.reference_name)
            .and_then(|events| events.iter().rfind(|e| e.line <= r.line).copied());
        Ok(last.is_some_and(|e| e.defined == Some(true)))
    }

    /// Cache syntax, never conditional truth.
    fn summarize(&mut self, file: &str) -> Rc<Vec<FileEvent>> {
        if let Some(s) = self.cpp_macros.summaries.get(file) {
            return s.clone();
        }
        let source = self.read_file(file).map(|f| f.join("\n")).unwrap_or_default();
        let lines = directive_lines(&source);
        let mut events = Vec::new();
        for (i, text) in lines.iter().enumerate() {
            let line = i as i64 + 1;
            if let Some(b) = re!(r"^\s*#\s*(ifdef|ifndef|if|elif|else|endif)\b(.*)$").captures(text) {
                let (op, expression) = (b[1].to_string(), b[2].to_string());
                let guard = guards_itself(&lines, i, &op, &expression);
                events.push(FileEvent::Branch { op, expression, guard, line });
                continue;
            }
            if let Some(d) = re!(r"^\s*#\s*(define|undef)\s+(\w+)(\(?)").captures(text) {
                let name = d[2].to_string();
                let function_like = &d[3] == "(";
                events.push(FileEvent::Define {
                    define: &d[1] == "define",
                    wraps_itself: function_like && calls_itself(&lines, i, &name),
                    value: text[d.get(0).map_or(0, |m| m.end())..].to_string(),
                    name,
                    line,
                    function_like,
                });
                continue;
            }
            if let Some(inc) = re!(r#"^\s*#\s*(?:include|import)\s*([<"])([^>"]+)[>"]"#).captures(text) {
                let quote = if &inc[1] == "\"" { '"' } else { '<' };
                events.push(FileEvent::Include { quote, spec: inc[2].to_string(), line });
            }
            if re!(r"^\s*#\s*pragma\s+once\b").is_match(text) {
                events.push(FileEvent::Once { line });
            }
        }
        let events = Rc::new(events);
        self.cpp_macros.summaries.insert(file.to_string(), events.clone());
        events
    }

    pub(super) fn resolve_cpp_include(&mut self, file: &str, quote: char, spec: &str, language: &str) -> Res<Option<String>> {
        let key = format!("{language}\0{file}\0{quote}{spec}");
        if let Some(hit) = self.cpp_macros.includes.get(&key) {
            return Ok(hit.clone());
        }
        let normalized = spec.replace('\\', "/");
        let local = pos_join(pos_dirname(file), &normalized);
        let mut target = if quote == '"' && !local.starts_with("../") && !local.starts_with('/') && self.file_exists(&local) {
            Some(local)
        } else {
            self.resolve_import_path(spec, file, language)?
        };
        if target.is_none() {
            if self.cpp_macros.by_basename.is_none() {
                let files: Vec<String> = match self.sorted_files() {
                    Some(files) => files.to_vec(),
                    None => self.table()?.files.iter().cloned().collect(),
                };
                let mut index: HashMap<String, Vec<String>> = HashMap::new();
                for f in files {
                    index.entry(pos_basename(&f).to_string()).or_default().push(f);
                }
                self.cpp_macros.by_basename = Some(index);
            }
            let suffix = format!("/{normalized}");
            let matches: Vec<&String> = self
                .cpp_macros
                .by_basename
                .as_ref()
                .and_then(|index| index.get(pos_basename(&normalized)))
                .map(|list| list.iter().filter(|f| **f == normalized || f.ends_with(&suffix)).collect())
                .unwrap_or_default();
            if matches.len() == 1 {
                target = Some(matches[0].clone());
            }
        }
        self.cpp_macros.includes.insert(key, target.clone());
        Ok(target)
    }

    fn walk_translation_unit(&mut self, root_file: &str, language: &str) -> RootTimeline {
        let mut walk = TuWalk::default();
        self.scan_tu_file(&mut walk, root_file, Some(true), None, language);
        RootTimeline { events: walk.timeline, cutoff: walk.cutoff }
    }

    fn scan_tu_file(&mut self, walk: &mut TuWalk, file: &str, inherited: Truth, include_line: Option<i64>, language: &str) {
        if walk.stopped || walk.cutoff.is_some() || inherited == Some(false) || walk.once.get(file) == Some(&Some(true)) {
            return;
        }
        if walk.scanning.contains(file) {
            walk.cuts.push(file.to_string());
            return;
        }
        if let Some(seen) = walk.visits.get(file) {
            if seen.inherited == inherited
                && seen.start == seen.end
                && seen.end == walk.version
                && seen.cuts.iter().all(|c| walk.scanning.contains(c))
            {
                let cuts = seen.cuts.clone();
                walk.cuts.extend(cuts);
                return;
            }
        }
        let start = walk.version;
        let first_cut = walk.cuts.len();
        walk.scanning.insert(file.to_string());
        let mut active = inherited;
        // (parent, taken)
        let mut frames: Vec<(Truth, Truth)> = Vec::new();
        let mut arms: Vec<Arms> = Vec::new();
        for ev in self.summarize(file).iter() {
            if walk.stopped {break;}
            if walk.cutoff.is_none() {
                walk.spent += 1;
                if walk.spent > WALK_EVENT_BUDGET {
                    walk.cutoff = Some(include_line.unwrap_or(ev.line()));
                }
            }
            if walk.cutoff.is_some() {break;}
            if walk.stop_at.as_ref().is_some_and(|(target,limit)|target==file && matches!(ev,FileEvent::Define{line,..}|FileEvent::Include{line,..} if line>limit)) {break;}
            match ev {
                FileEvent::Branch { op, expression, guard, .. } => {
                    match op.as_str() {
                        "if" | "ifdef" | "ifndef" => {
                            let known = walk.definitions.get(expression.trim()).and_then(|d| d.defined);
                            let mut selected = match op.as_str() {
                                "if" => walk.condition(expression),
                                "ifndef" => not(known),
                                _ => known,
                            };
                            if selected.is_none() && *guard {
                                selected = Some(true);
                            }
                            frames.push((active, selected));
                            arms.push(Arms::default());
                            active = and(active, selected);
                        }
                        "endif" => {
                            active = frames.pop().map_or(inherited, |f| f.0);
                            arms.pop();
                        }
                        _ => {
                            if let Some(a) = arms.last_mut() {
                                a.arm += 1;
                                a.in_else = op == "else";
                            }
                            let test = if op == "else" { Some(true) } else { walk.condition(expression) };
                            if let Some(frame) = frames.last_mut() {
                                active = and(frame.0, and(not(frame.1), test));
                                frame.1 = or(frame.1, test);
                            }
                        }
                    }
                    continue;
                }
                _ if active == Some(false) => continue,
                FileEvent::Once { .. } => {
                    let prior = walk.once.get(file).copied().unwrap_or(Some(false));
                    let next = or(prior, active);
                    if next != prior {
                        walk.version += 1;
                    }
                    walk.once.insert(file.to_string(), next);
                }
                FileEvent::Include { quote, spec, line } => {
                    // A resolution error only loses this include's macros.
                    if let Ok(Some(target)) = self.resolve_cpp_include(file, *quote, spec, language) {
                        self.scan_tu_file(walk, &target, active, Some(include_line.unwrap_or(*line)), language);
                    }
                }
                FileEvent::Define { define, name, line, function_like, value, wraps_itself } => {
                    let prior = walk.definitions.get(name).cloned();
                    let is_macro = *define && *function_like && !*wraps_itself;
                    // An `#else` that ends a run of arms which all agree settles
                    // the name whichever arm the build takes.
                    let exhaustive = match (frames.last(), arms.last_mut()) {
                        (Some(frame), Some(a)) => {
                            let e = a.defs.entry(name.clone()).or_insert((usize::MAX, 0, Some(is_macro)));
                            if e.0 != a.arm {
                                e.0 = a.arm;
                                e.1 += 1;
                            }
                            if e.2 != Some(is_macro) {
                                e.2 = None;
                            }
                            a.in_else && frame.0 == Some(true) && e.1 == a.arm + 1 && e.2 == Some(is_macro)
                        }
                        _ => false,
                    };
                    let now = if active == Some(true) || exhaustive || prior.as_ref().and_then(|p| p.is_macro) == Some(is_macro) {
                        Some(is_macro)
                    } else {
                        None
                    };
                    if let Some(aliases)=&mut walk.type_aliases {
                        let before=aliases.get(name).cloned();
                        let value=value.trim();
                        let simple=*define && !*function_like && re!(r"^[A-Za-z_]\w*$").is_match(value);
                        if let Some(arm)=arms.last_mut() {
                            let entry=arm.aliases.entry(name.clone()).or_insert_with(||(HashSet::new(),false));
                            if simple {entry.0.insert(value.to_string());} else {entry.1=true;}
                        }
                        if active==Some(true) {
                            if simple {aliases.insert(name.clone(),(HashSet::from([value.to_string()]),false));} else {aliases.remove(name);}
                        } else if exhaustive {
                            if let Some(entry)=arms.last().and_then(|a|a.aliases.get(name)) {aliases.insert(name.clone(),entry.clone());}
                        } else if simple {
                            aliases.entry(name.clone()).or_insert_with(||(HashSet::new(),true)).0.insert(value.to_string());
                        } else if let Some(entry)=aliases.get_mut(name) {entry.1=true;}
                        if aliases.get(name)!=before.as_ref() {walk.version+=1;}
                    }
                    // A name the walk has not seen is an unknown build flag,
                    // as `#ifdef` reads it: an `#undef` under an unknown
                    // condition leaves it unknown, not undefined (upstream
                    // takes it as undefined, which let CPython's Windows
                    // `#if Py_GIL_DISABLED == 0` / `#undef` pick the
                    // non-free-threaded branch of every file for certain).
                    let prior_defined = prior.and_then(|p| p.defined);
                    let entry = Definition {
                        defined: if *define {
                            or(prior_defined, active)
                        } else {
                            and(prior_defined, not(active))
                        },
                        value: if *define && active == Some(true) { Some(Rc::from(value.as_str())) } else { None },
                        is_macro: now,
                    };
                    if walk.definitions.get(name) != Some(&entry) {
                        walk.version += 1;
                    }
                    walk.definitions.insert(name.clone(), entry);
                    if *function_like && walk.macro_names.insert(name.clone()) {
                        walk.version += 1;
                    }
                    if walk.macro_names.contains(name) {
                        let line = include_line.unwrap_or(*line);
                        walk.timeline.entry(name.clone()).or_default().push(Event { line, defined: now });
                    }
                }
            }
        }
        if walk.stop_at.as_ref().is_some_and(|(target,_)|target==file) {walk.stopped=true;}
        walk.scanning.remove(file);
        let mut own: Vec<String> = Vec::new();
        for cut in walk.cuts.split_off(first_cut) {
            if !own.contains(&cut) {
                own.push(cut);
            }
        }
        walk.cuts.extend(own.iter().cloned());
        walk.visits.insert(file.to_string(), Visit { inherited, start, end: walk.version, cuts: own });
    }

    /// Possible simple type aliases at a declaration, evaluated in its caller's import context.
    pub(super) fn cpp_type_alias_names(&mut self,r:&ResolveRefIn,file:&str,line:i64,name:&str)->Vec<String> {
        let key=format!("{}\0{}\0{}",r.file_path,file,line);
        let aliases=match self.cpp_macros.type_aliases.iter().find(|(k,_)|k==&key) {
            Some((_,aliases))=>aliases.clone(),
            None=>{
                let mut walk=TuWalk{type_aliases:Some(HashMap::new()),stop_at:Some((file.to_string(),line)),..TuWalk::default()};
                self.scan_tu_file(&mut walk,&r.file_path,Some(true),None,&r.language);
                // A walk the budget cut off never saw the declaration's whole context.
                let aliases=Rc::new(if walk.stopped && walk.cutoff.is_none() {walk.type_aliases.unwrap_or_default()} else {HashMap::new()});
                if self.cpp_macros.type_aliases.len()>=ROOT_TIMELINE_CAP {self.cpp_macros.type_aliases.pop_front();}
                self.cpp_macros.type_aliases.push_back((key,aliases.clone()));aliases
            }
        };
        let mut queue=VecDeque::from([(name.to_string(),0)]);let mut seen=HashSet::new();let mut result=HashSet::new();
        while let Some((name,depth))=queue.pop_front() {
            if depth>=ALIAS_DEPTH || !seen.insert(name.clone()) {continue;}
            if let Some((targets,literal))=aliases.get(&name) {
                if *literal {result.insert(name);}
                queue.extend(targets.iter().cloned().map(|name|(name,depth+1)));
            } else {result.insert(name);}
        }
        result.into_iter().collect()
    }

    /// matchCppConstructor: `ns::T::T/<arity>` → the single admitting
    /// constructor of the lexically nearest `T`; `None` for an aggregate, an
    /// ambiguous overload set or an initializer_list overload.
    pub(super) fn match_cpp_constructor(&mut self, r: &ResolveRefIn) -> Res<Option<KCand>> {
        let Some(c) = re!(r"^(.*)::([^:]+)/(\d+)$").captures(&r.reference_name) else {
            return Ok(None);
        };
        let raw_type = &c[1];
        let name = &c[2];
        let Ok(argc) = c[3].parse::<usize>() else { return Ok(None) };
        let ty = raw_type.strip_prefix("::").unwrap_or(raw_type);
        if ty.rsplit("::").next() != Some(name) {
            return Ok(None);
        }
        let included = self.namespace_visible_files(&r.file_path, "cpp")?;
        if self.cpp_alias_expansion(raw_type, r, 0, &included)?.is_some() {
            let Some(owner) = self.cpp_type_owner(raw_type, r, 0, true)? else { return Ok(None); };
            let expanded = r.clone().naming(&format!("::{}::{}/{argc}", owner.qualified_name, owner.name), "calls");
            return self.match_cpp_constructor(&expanded);
        }
        // Innermost lexical namespace first, then outward, then global.
        let scopes: Vec<String> = if raw_type.starts_with("::") {
            Vec::new()
        } else {
            self.node_by_id(&r.from_node_id)?
                .map(|n| n.qualified_name.split("::").map(str::to_string).collect())
                .unwrap_or_default()
        };
        let mut qualified_names: Vec<String> = (1..=scopes.len()).rev().map(|i| format!("{}::{ty}", scopes[..i].join("::"))).collect();
        qualified_names.push(ty.to_string());

        for qualified in qualified_names {
            let named = self.nodes_by_qualified_name(&qualified)?;
            let owner_files: HashSet<String> = named
                .iter()
                .filter(|n| n.language == "cpp" && matches!(n.kind.as_str(), "class" | "struct" | "union"))
                .map(|n| n.file_path.clone())
                .collect();
            if owner_files.is_empty() {
                // `using T = int;` / `enum T` in a nearer scope hides an outer
                // class `T`: the declaration constructs no class there.
                if named.iter().any(|n| n.language == "cpp" && matches!(n.kind.as_str(), "type_alias" | "enum")) {
                    let Some(owner) = self.cpp_type_owner(&qualified, r, 0, true)? else { return Ok(None) };
                    let expanded = r.clone().naming(&format!("::{}::{}/{argc}", owner.qualified_name, owner.name), "calls");
                    return self.match_cpp_constructor(&expanded);
                }
                continue;
            }
            // Same-named types in different files are different types (a
            // class local to a .cpp, an anonymous namespace). A type the
            // calling file defines owns the call and only its own file's
            // constructors; otherwise more than one defining file is ambiguous.
            let local = owner_files.contains(&r.file_path);
            if !local && owner_files.len() > 1 {
                return Ok(None);
            }
            let ctor_qname = format!("{qualified}::{name}");
            let mut constructors: Vec<Arc<KNode>> = Vec::new();
            for n in self.nodes_by_name(name)?.iter() {
                if n.language != "cpp" || n.kind != "method" || n.qualified_name != ctor_qname {
                    continue;
                }
                if local && n.file_path != r.file_path {
                    continue;
                }
                // A deleted overload is never the one a valid program calls.
                if self.is_deleted_function(n) {
                    continue;
                }
                constructors.push(n.clone());
            }
            // Brace-init prefers an initializer_list overload over arity —
            // that choice needs the argument types, so decline. With no
            // arguments (`T obj;`, `T obj{}`) the default constructor wins.
            if argc > 0 && constructors.iter().any(|n| re!(r"\binitializer_list\b").is_match(n.signature.as_deref().unwrap_or(""))) {
                return Ok(None);
            }
            // A prototype and its out-of-line definition describe one
            // overload: merge their admissible ranges, then prefer the definition.
            let mut overloads: Vec<(String, Vec<Arc<KNode>>, usize, usize)> = Vec::new();
            for node in constructors {
                let Some((key, min, max)) = constructor_shape(node.signature.as_deref()) else {
                    return Ok(None);
                };
                match overloads.iter_mut().find(|o| o.0 == key) {
                    Some(prior) => {
                        prior.1.push(node);
                        prior.2 = prior.2.min(min);
                    }
                    None => overloads.push((key, vec![node], min, max)),
                }
            }
            let mut admitting = overloads.into_iter().filter(|o| o.2 <= argc && argc <= o.3);
            let (Some(only), None) = (admitting.next(), admitting.next()) else {
                return Ok(None);
            };
            let definitions: Vec<&Arc<KNode>> = only.1.iter().filter(|n| !n.signature.as_deref().unwrap_or("").ends_with(';')).collect();
            let targets: Vec<&Arc<KNode>> = if definitions.is_empty() { only.1.iter().collect() } else { definitions };
            return Ok(match targets.as_slice() {
                [one] => Some(KCand { node: (*one).clone(), confidence: 0.9, resolved_by: "qualified-name" }),
                _ => None,
            });
        }
        Ok(None)
    }
}

impl KernelResolver {
    /// `T(const T &) = delete;` — the declaration ends in `= delete`.
    fn is_deleted_function(&mut self, node: &KNode) -> bool {
        let Some(lines) = self.read_file(&node.file_path) else { return false };
        let lo = (node.start_line - 1).max(0) as usize;
        let hi = (node.end_line.max(node.start_line) as usize).min(lines.len());
        let text = lines.get(lo..hi).unwrap_or(&[]).join("\n");
        re!(r"\)[^;{]*=\s*delete\s*;").is_match(&text)
    }
}

/// isCppConstructorRef.
pub(super) fn is_cpp_constructor_ref(r: &ResolveRefIn) -> bool {
    r.language == "cpp" && r.reference_kind == "calls" && re!(r"::[^:]+/\d+$").is_match(&r.reference_name)
}

/// constructorShape: `(key, min, max)` admissible argument counts of a
/// `(params)` signature; `None` when it can't be read. `usize::MAX` = variadic.
fn constructor_shape(signature: Option<&str>) -> Option<(String, usize, usize)> {
    let signature = signature?;
    let signature = signature.strip_suffix(';').unwrap_or(signature);
    let text = signature.strip_prefix('(')?.strip_suffix(')')?.trim();
    if text.is_empty() || text == "void" {
        return Some((String::new(), 0, 0));
    }
    // Split on top-level commas only: `std::map<K, V>`, `int (*cb)(int, int)`
    // and `T x = f(a, b)` all nest their commas.
    let bytes = text.as_bytes();
    let mut parts: Vec<&str> = Vec::new();
    let (mut start, mut depth, mut quote) = (0usize, 0i32, 0u8);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if quote != 0 {
            if c == b'\\' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else {
            if b"(<[{".contains(&c) {
                depth += 1;
            }
            if b")>]}".contains(&c) {
                depth -= 1;
            }
            if c == b',' && depth == 0 {
                parts.push(&text[start..i]);
                start = i + 1;
            }
        }
        i += 1;
    }
    if depth != 0 || quote != 0 {
        return None;
    }
    parts.push(&text[start..]);
    // A comparison in a default argument would be mistaken for a `=` default
    // or a template bracket — leave those to a compiler.
    if parts.iter().any(|p| re!(r"[<>]=|==|!=").is_match(p)) {
        return None;
    }
    let variadic = parts.iter().any(|p| p.contains("..."));
    let types: Vec<String> = parts
        .iter()
        .map(|p| {
            let ty = p.split('=').next().unwrap_or("").trim();
            // Strip an optional parameter name, keeping unnamed built-in
            // types (`unsigned int`) and qualifiers; uncertainty must not
            // merge overloads.
            let ty = match re!(r"^(.*[\s*&>])([A-Za-z_]\w*)$").captures(ty) {
                Some(c)
                    if !re!(r"^(?:void|bool|char|short|int|long|float|double|signed|unsigned|const|volatile)$").is_match(&c[2])
                        && !re!(r"^(?:const|volatile|struct|class|enum)\s*$").is_match(&c[1]) =>
                {
                    c[1].to_string()
                }
                _ => ty.to_string(),
            };
            ty.split_whitespace().collect()
        })
        .collect();
    let min = parts.iter().filter(|p| !p.contains('=') && !p.contains("...")).count();
    let max = if variadic { usize::MAX } else { parts.len() };
    Some((types.join(","), min, max))
}

/// `text` with the contents of its string and char literals turned to spaces,
/// so a name inside `"TRACE(%d)"` is not read as a call. Offsets are kept.
fn blank_literals(text: &str) -> String {
    let mut out = text.as_bytes().to_vec();
    let mut quote = None;
    let mut k = 0;
    while k < out.len() {
        let c = out[k];
        match quote {
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None => {}
            Some(q) if c == q => quote = None,
            Some(_) => {
                out[k] = b' ';
                if c == b'\\' && k + 1 < out.len() {
                    k += 1;
                    out[k] = b' ';
                }
            }
        }
        k += 1;
    }
    // Every byte of a literal's contents was replaced, so the rest is intact UTF-8.
    String::from_utf8(out).unwrap_or_default()
}

/// Does the body of the `#define NAME(` at `index` (continuation lines
/// included) call `NAME`? A wrapper macro that calls its own name is how that
/// function gets called and hides nothing.
fn calls_itself(lines: &[String], index: usize, name: &str) -> bool {
    let mut text = lines[index].clone();
    let mut j = index;
    while j + 1 < lines.len() && re!(r"\\\s*$").is_match(&lines[j]) {
        text.push(' ');
        text.push_str(&lines[j + 1]);
        j += 1;
    }
    let body = blank_literals(&text[text.find('(').map_or(text.len(), |i| i + 1)..]);
    let body = body.as_str();
    let b = body.as_bytes();
    let skip_ws = |mut k: usize| {
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        k
    };
    let skip_ws_back = |mut k: usize| {
        while k > 0 && b[k - 1].is_ascii_whitespace() {
            k -= 1;
        }
        k
    };
    for (at, _) in body.match_indices(name) {
        let end = at + name.len();
        // `NAME(` as a whole word.
        if (at == 0 || !is_word(b[at - 1])) && b.get(skip_ws(end)) == Some(&b'(') {
            return true;
        }
        // `(NAME)(`.
        let before = skip_ws_back(at);
        if before > 0 && b[before - 1] == b'(' {
            let close = skip_ws(end);
            if b.get(close) == Some(&b')') && b.get(skip_ws(close + 1)) == Some(&b'(') {
                return true;
            }
        }
    }
    false
}

/// The include-guard idiom: `#ifndef X_H` (or `#if !defined(X_H)`) whose next
/// directive is `#define X_H` — or the same shape around a fallback
/// function-like macro. A default VALUE (`#define ENABLE_X 0`) is the flag a
/// build overrides, so it stays unknown.
fn guards_itself(lines: &[String], index: usize, op: &str, expression: &str) -> bool {
    let name = if op == "ifndef" {
        Some(expression.trim())
    } else {
        re!(r"^\s*!\s*defined\s*(?:\(\s*(\w+)\s*\)|(\w+))\s*$")
            .captures(expression)
            .and_then(|c| c.get(1).or_else(|| c.get(2)))
            .map(|m| m.as_str())
    };
    let Some(name) = name.filter(|n| re!(r"^\w+$").is_match(n)) else { return false };
    let Some(define) = lines[index + 1..].iter().position(|t| re!(r"^\s*#").is_match(t)).map(|k| index + 1 + k) else {
        return false;
    };
    let Some(c) = re!(r"^\s*#\s*define\s+(\w+)(.*)$").captures(&lines[define]) else { return false };
    if &c[1] != name {
        return false;
    }
    c[2].trim().is_empty() || c[2].starts_with('(') || wraps_file(lines, index, define)
}

/// `#define X_H 1` is a guard, not a flag default, when its `#ifndef` is the
/// file's first conditional and the matching `#endif` its last directive,
/// with more directives inside: the shape compilers treat as an include guard.
fn wraps_file(lines: &[String], index: usize, define: usize) -> bool {
    let directive = |t: &String| re!(r"^\s*#").is_match(t);
    if lines[..index].iter().any(|t| directive(t) && !re!(r"^\s*#\s*pragma\b").is_match(t)) {
        return false;
    }
    let mut depth = 0usize;
    let mut inner = false;
    for (k, t) in lines.iter().enumerate().skip(index) {
        if !directive(t) {
            continue;
        }
        if re!(r"^\s*#\s*if(?:n?def)?\b").is_match(t) {
            depth += 1;
        } else if re!(r"^\s*#\s*endif\b").is_match(t) {
            depth -= 1;
            if depth == 0 {
                return inner && !lines[k + 1..].iter().any(directive);
            }
        } else if depth == 1 && re!(r"^\s*#\s*(?:elif|else)\b").is_match(t) {
            return false;
        }
        if k > define {
            inner = true;
        }
    }
    false
}

/// The file's lines with comments removed only as far as the preprocessor
/// needs: a line inside a block comment is blank, a directive line loses its
/// trailing `//` / `/* … */`, and every other line is kept verbatim.
fn directive_lines(source: &str) -> Vec<String> {
    let masked = mask_cpp_raw_strings(source);
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in masked.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let mut text = raw.to_string();
        let opened_in_comment = in_block;
        if in_block {
            match text.find("*/") {
                None => {
                    out.push(String::new());
                    continue;
                }
                Some(end) => {
                    text = text[end + 2..].to_string();
                    in_block = false;
                }
            }
        }
        let directive = re!(r"^\s*#").is_match(&text);
        let mut kept: Option<String> = None;
        let mut quote = 0u8;
        let mut i = 0;
        while i < text.len() {
            let c = text.as_bytes()[i];
            if quote != 0 {
                if c == b'\\' {
                    i += 1;
                } else if c == quote {
                    quote = 0;
                }
                i += 1;
                continue;
            }
            if c == b'"' || c == b'\'' {
                quote = c;
                i += 1;
                continue;
            }
            let next = text.as_bytes().get(i + 1).copied();
            if c == b'/' && next == Some(b'/') {
                kept = Some(text[..i].to_string());
                break;
            }
            if c == b'/' && next == Some(b'*') {
                match text[i + 2..].find("*/") {
                    None => {
                        in_block = true;
                        kept = Some(text[..i].to_string());
                        break;
                    }
                    Some(rel) => {
                        text = format!("{} {}", &text[..i], &text[i + 2 + rel + 2..]);
                        continue;
                    }
                }
            }
            i += 1;
        }
        out.push(if directive {
            kept.filter(|k| !k.is_empty()).unwrap_or(text)
        } else if opened_in_comment {
            // `#define x … */`: the part before `*/` is comment.
            kept.unwrap_or(text)
        } else {
            raw.to_string()
        });
    }
    out
}

/// maskCppRawStrings (c-cpp.ts): blank every raw string literal
/// (`R"delim(…)delim"`) except its newlines, skipping comments and ordinary
/// literals, so a `#define` inside one is never read as a directive.
pub(super) fn mask_cpp_raw_strings(source: &str) -> std::borrow::Cow<'_, str> {
    if !source.contains("R\"") {
        return std::borrow::Cow::Borrowed(source);
    }
    let b = source.as_bytes();
    let find = |from: usize, needle: &[u8]| b[from.min(b.len())..].windows(needle.len()).position(|w| w == needle).map(|k| from + k);
    let skip_quoted = |mut k: usize, q: u8| {
        k += 1;
        while k < b.len() {
            if b[k] == b'\\' {
                k += 2;
                continue;
            }
            if b[k] == q {
                return k + 1;
            }
            k += 1;
        }
        b.len()
    };
    let mut out = b.to_vec();
    let mut changed = false;
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            i = find(i, b"\n").unwrap_or(b.len());
            continue;
        }
        if b[i..].starts_with(b"/*") {
            i = find(i + 2, b"*/").map_or(b.len(), |k| k + 2);
            continue;
        }
        if i == 0 || !is_word(b[i - 1]) {
            let prefix = if b[i..].starts_with(b"u8") { 2 } else if matches!(b[i], b'L' | b'u' | b'U') { 1 } else { 0 };
            let at = i + prefix;
            if b[at..].starts_with(b"R\"") {
                let open = at + 2;
                let delim = b[open..]
                    .iter()
                    .take(17)
                    .position(|&c| matches!(c, b' ' | b'\t' | 0x0b | 0x0c | b'\r' | b'\n' | b'(' | b')' | b'\\'))
                    .filter(|&n| n <= 16 && b[open + n] == b'(');
                if let Some(n) = delim {
                    let mut closer = vec![b')'];
                    closer.extend_from_slice(&b[open..open + n]);
                    closer.push(b'"');
                    let end = find(open + n + 1, &closer).map_or(b.len(), |k| k + closer.len());
                    for byte in &mut out[i..end] {
                        if *byte != b'\n' && *byte != b'\r' {
                            *byte = 0;
                        }
                    }
                    changed = true;
                    i = end;
                    continue;
                }
            }
            if b.get(at) == Some(&b'\'') {
                i = skip_quoted(at, b'\'');
                continue;
            }
        }
        if b[i] == b'"' {
            i = skip_quoted(i, b'"');
            continue;
        }
        i += 1;
    }
    if !changed {
        return std::borrow::Cow::Borrowed(source);
    }
    // Only whole literals are masked (they start and end on ASCII), so the
    // bytes stay valid UTF-8; the fallback is unreachable in practice.
    std::borrow::Cow::Owned(String::from_utf8(out).unwrap_or_else(|_| source.to_string()))
}
