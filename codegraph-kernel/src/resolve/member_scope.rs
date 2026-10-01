//! Where a member reached by name alone can land in VB.NET, C# and
//! Objective-C (name-matcher.ts isVbMemberReachable, isCsharpMemberInScope,
//! isObjcSelfSendTarget and objcReceiverReaches). The extractors of these
//! languages drop what a member access is written on, so the line says what
//! the name can mean. Each rule only removes candidates; none picks one.

use super::*;

fn is_vb_member_kind(kind: &str) -> bool {
    matches!(kind, "method" | "property" | "field" | "enum_member" | "constant" | "variable")
}

fn is_csharp_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "interface" | "enum" | "struct" | "record")
}

fn is_csharp_member_kind(kind: &str) -> bool {
    matches!(kind, "method" | "property" | "field" | "enum_member" | "constant" | "event")
}

fn is_objc_member_kind(kind: &str) -> bool {
    matches!(kind, "method" | "property" | "field")
}

/// The simple name of the type that owns a member: the last segment of its
/// qualified name before the member (`App.Specs::SpecBase` → `SpecBase`).
fn owner_simple_name(n: &KNode) -> Option<&str> {
    let cut = n.qualified_name.rfind("::")?;
    let prefix = &n.qualified_name[..cut];
    let from = prefix.rfind("::").map(|i| i + 2).into_iter().chain(prefix.rfind('.').map(|i| i + 1)).max().unwrap_or(0);
    Some(&prefix[from..])
}

/// Byte offsets of `name` on `line` with no word byte (or `.` when
/// `dot_bounded`) before it and no word byte after it.
fn word_occurrences<'a>(line: &'a str, name: &'a str, dot_bounded: bool) -> impl Iterator<Item = usize> + 'a {
    affix::occurrences(line, name, 0).filter(move |&at| {
        let bytes = line.as_bytes();
        let before_ok = at == 0 || !(affix::is_word_byte(bytes[at - 1]) || (dot_bounded && bytes[at - 1] == b'.'));
        let end = at + name.len();
        before_ok && (end >= bytes.len() || !affix::is_word_byte(bytes[end]))
    })
}

/// How a bare Objective-C name is written at its site.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ObjcShape {
    /// C call syntax (`completionBlock()`): a function, block or function
    /// pointer, never a method or property.
    CCall,
    /// A message to `self` / `super` / `[self class]`, whose receiver the
    /// extractor drops.
    SelfSend,
    SuperSend,
}

/// UIKit / AppKit superclasses, for a category on a system class: an
/// `UIImageView (WebCache)` method sending `[self sd_internalSetImageWithURL:…]`
/// reaches the `UIView (WebCache)` category.
pub(super) const OBJC_SYSTEM_SUPERS: &[(&str, &str)] = &[
    ("UISegmentedControl", "UIControl"), ("UIStepper", "UIControl"), ("UIPageControl", "UIControl"),
    ("UIDatePicker", "UIControl"), ("UIRefreshControl", "UIControl"), ("UIStackView", "UIView"),
    ("UINavigationBar", "UIView"), ("UIToolbar", "UIView"), ("UITabBar", "UIView"), ("UISearchBar", "UIView"),
    ("UIVisualEffectView", "UIView"), ("UIActivityIndicatorView", "UIView"), ("UIProgressView", "UIView"),
    ("UIPickerView", "UIView"), ("UITableViewHeaderFooterView", "UIView"),
    ("UITableViewController", "UIViewController"), ("UICollectionViewController", "UIViewController"),
    ("UINavigationController", "UIViewController"), ("UITabBarController", "UIViewController"),
    ("UIPageViewController", "UIViewController"), ("UISplitViewController", "UIViewController"),
    ("UIAlertController", "UIViewController"), ("UIHostingController", "UIViewController"),
    ("UIResponder", "NSObject"), ("UIView", "UIResponder"), ("UIViewController", "UIResponder"), ("UIWindow", "UIView"),
    ("UIControl", "UIView"), ("UIButton", "UIControl"), ("UITextField", "UIControl"), ("UISwitch", "UIControl"),
    ("UISlider", "UIControl"), ("UIImageView", "UIView"), ("UILabel", "UIView"), ("UIScrollView", "UIView"),
    ("UITableView", "UIScrollView"), ("UICollectionView", "UIScrollView"), ("UITextView", "UIScrollView"),
    ("UITableViewCell", "UIView"), ("UICollectionReusableView", "UIView"),
    ("UICollectionViewCell", "UICollectionReusableView"), ("MKAnnotationView", "UIView"), ("MKMapView", "UIView"),
    ("NSResponder", "NSObject"), ("NSView", "NSResponder"), ("NSViewController", "NSResponder"),
    ("NSWindow", "NSResponder"), ("NSControl", "NSView"), ("NSImageView", "NSControl"), ("NSButton", "NSControl"),
    ("NSTextField", "NSControl"), ("NSTableView", "NSControl"),
];

/// The per-ref facts the three rules read, computed once per ref.
pub(super) struct MemberSite {
    /// VB.NET: what the member access is written on (`None` for a bare name).
    vb_receiver: Option<String>,
    /// VB.NET: the receiver is a service locator's `(Of T)` type argument.
    vb_type_arg: bool,
    csharp_bare: bool,
    objc_shape: Option<ObjcShape>,
}

impl MemberSite {
    pub(super) fn is_judged(&self) -> bool {
        self.vb_receiver.is_some() || self.csharp_bare || self.objc_shape.is_some()
    }
}

impl KernelResolver {
    pub(super) fn member_site(&mut self, r: &ResolveRefIn) -> MemberSite {
        let word = re!(r"^[A-Za-z0-9_]+$").is_match(&r.reference_name);
        let (vb_receiver, vb_type_arg) =
            match (r.language == "vbnet" && matches!(r.reference_kind.as_str(), "calls" | "instantiates") && word)
                .then(|| self.vb_receiver_of(r))
                .flatten()
            {
                Some((receiver, type_arg)) => (Some(receiver), type_arg),
                None => (None, false),
            };
        let csharp_bare = r.language == "csharp"
            && matches!(r.reference_kind.as_str(), "calls" | "references")
            && re!(r"^[A-Za-z_][A-Za-z0-9_]*$").is_match(&r.reference_name);
        let objc_shape = (r.language == "objc"
            && r.reference_kind == "calls"
            && re!(r"^[A-Za-z_][A-Za-z0-9_]*:*(?:[A-Za-z0-9_]+:)*$").is_match(&r.reference_name))
        .then(|| self.objc_call_shape(r))
        .flatten();
        MemberSite { vb_receiver, vb_type_arg, csharp_bare, objc_shape }
    }

    /// Whether the member rules let `n` stand for the name at `site`.
    pub(super) fn is_member_in_reach(&mut self, n: &KNode, site: &MemberSite, r: &ResolveRefIn) -> Res<bool> {
        if let Some(receiver) = &site.vb_receiver {
            if !is_vb_member_reachable(n, receiver) {
                return Ok(false);
            }
        }
        if site.csharp_bare && !self.is_csharp_member_in_scope(n, r)? {
            return Ok(false);
        }
        match site.objc_shape {
            Some(ObjcShape::CCall) if is_objc_member_kind(&n.kind) => Ok(false),
            Some(ObjcShape::SelfSend) => self.is_objc_self_send_target(n, r, false),
            Some(ObjcShape::SuperSend) => self.is_objc_self_send_target(n, r, true),
            _ => Ok(true),
        }
    }

    /// Whether the one candidate the member rules left is bound to the site
    /// rather than merely left over: a declaration in the calling file, a
    /// member of the type the VB receiver names (`Logger.Log`, `(Of T)`) or of
    /// the class a `Me.` / `self` / bare C# name is written in or inherits.
    /// A VB receiver the file also declares as a variable (`Dim logger As
    /// New FileLogger()`) names a value, not the type its spelling matches.
    /// Anything else stays below the trusted range.
    pub(super) fn is_member_survivor_bound(&mut self, n: &KNode, site: &MemberSite, r: &ResolveRefIn) -> Res<bool> {
        if n.file_path == r.file_path {
            return Ok(true);
        }
        if let Some(receiver) = &site.vb_receiver {
            if !is_vb_member_kind(&n.kind) {
                return Ok(false);
            }
            let Some(owner) = owner_simple_name(n).map(str::to_ascii_lowercase) else { return Ok(false) };
            let last = receiver.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            if last == owner && (site.vb_type_arg || !self.vb_declares_variable(&r.file_path, &last)?) {
                return Ok(true);
            }
            // `Me.X` in one part of a partial class reaches the other parts.
            let enclosing = self
                .nodes_in_file(&r.file_path)?
                .iter()
                .filter(|t| matches!(t.kind.as_str(), "class" | "struct" | "module") && t.start_line <= r.line && t.end_line >= r.line)
                .min_by_key(|t| t.end_line - t.start_line)
                .map(|t| t.name.to_ascii_lowercase());
            return Ok(enclosing.is_some_and(|e| e == owner));
        }
        if site.csharp_bare {
            return Ok(is_csharp_member_kind(&n.kind) && self.csharp_member_verdict(n, r)? == Some(true));
        }
        if matches!(site.objc_shape, Some(ObjcShape::SelfSend | ObjcShape::SuperSend)) && is_objc_member_kind(&n.kind) {
            let Some(owner) = n.qualified_name.rfind("::").map(|cut| n.qualified_name[..cut].to_string()) else {
                return Ok(false);
            };
            return Ok(self.objc_hierarchy_at(r)?.is_some_and(|h| h.contains(&owner)));
        }
        Ok(false)
    }

    // -- VB.NET ---------------------------------------------------------------

    /// vbReceiverOf: what a VB.NET member access is written on, read at the
    /// call site (the extractor keeps a call's last name only). `None` for a
    /// genuinely bare name, `""` for a `With` block's `.Name`, else the text
    /// before the dot (`Me.CMB.Buttons`, `System.Drawing`) or a service
    /// locator's type argument (`GetService(Of Notifier).Notify()`).
    fn vb_receiver_of(&mut self, r: &ResolveRefIn) -> Option<(String, bool)> {
        let lines = self.read_file(&r.file_path)?;
        let line = lines.get((r.line - 1).max(0) as usize)?;
        let lower = line.to_ascii_lowercase();
        let name = r.reference_name.to_ascii_lowercase();
        let column = r.column.max(0) as usize;
        let start = if names::js_slice(&lower, column).starts_with(&name) {
            Some(names::js_unit_to_byte(&lower, column))
        } else {
            word_occurrences(&lower, &name, false).next()
        }?;
        let before = &line[..start];
        let dot = re!(r"([A-Za-z0-9_.()]*?)\s*\.\s*$").captures(before)?;
        if let Some(t) = re!(r"(?i)\(\s*Of\s+([A-Za-z0-9_.]+)\s*\)\s*\.\s*$").captures(before) {
            return Some((t[1].to_string(), true));
        }
        Some((re!(r"\([^()]*\)").replace_all(&dot[1], "").into_owned(), false))
    }

    /// Whether a VB.NET file declares `name` (lowercase) as a variable,
    /// parameter, field or property: `Dim logger`, `ByVal logger`,
    /// `Property logger` or `logger As …`.
    pub(super) fn vb_declares_variable(&mut self, file: &str, name: &str) -> Res<bool> {
        let Some(src) = self.read_file(file) else { return Ok(false) };
        let e = regex::escape(name);
        let decl = Self::cached_regex(&format!(
            r"(?i)(?:(?-u:\b)(?:Dim|ByVal|ByRef|Property)\s+{e}(?-u:\b)|(?-u:\b){e}\s+As(?-u:\b))"
        ))?;
        Ok(decl.is_match(src.text()))
    }

    // -- C# -------------------------------------------------------------------

    /// isCsharpMemberInScope: a bare C# name means a member of the types
    /// around it (outer classes included), of their base types, or of a type
    /// a static using brings in. Never an unrelated class's: eShop's
    /// `TestContext.Current` in one test class went to another test class's
    /// `TestContext` property. A chain link the line shows a receiver for,
    /// and a name outside any recovered type, are not judged.
    fn is_csharp_member_in_scope(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<bool> {
        if !is_csharp_member_kind(&n.kind) {
            return Ok(true);
        }
        Ok(self.csharp_member_verdict(n, r)?.unwrap_or(true))
    }

    /// `Some(true)` when the scope binds the member, `Some(false)` when it
    /// rules it out, `None` when the rule does not judge the site.
    fn csharp_member_verdict(&mut self, n: &KNode, r: &ResolveRefIn) -> Res<Option<bool>> {
        let Some(owner) = owner_simple_name(n).map(str::to_string) else { return Ok(None) };
        if !self.has_no_receiver_on_line(r) {
            return Ok(None);
        }
        let mut queue: VecDeque<String> = self
            .nodes_in_file(&r.file_path)?
            .iter()
            .filter(|t| is_csharp_type_kind(&t.kind) && t.start_line <= r.line && t.end_line >= r.line)
            .map(|t| t.name.clone())
            .collect();
        // No type around the name means its declaration wasn't recovered.
        let enclosed = !queue.is_empty();
        let mut seen: HashSet<String> = HashSet::new();
        while seen.len() < 40 {
            let Some(name) = queue.pop_front() else { break };
            if !seen.insert(name.clone()) {
                continue;
            }
            if name == owner {
                return Ok(Some(true));
            }
            queue.extend(self.csharp_supertypes_of(&name)?.iter().cloned());
        }
        if self.csharp_static_usings(&r.file_path)?.contains(&owner) {
            return Ok(Some(true));
        }
        Ok(enclosed.then_some(false))
    }

    /// csharpSupertypesOf: the simple names a C# type's declarations (every
    /// `partial` one) derive from, read from `class X : Base, IFoo` heads.
    fn csharp_supertypes_of(&mut self, type_name: &str) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.csharp_supers_memo.get(type_name) {
            return Ok(hit.clone());
        }
        let decls: Vec<Arc<KNode>> = self
            .nodes_by_name(type_name)?
            .iter()
            .filter(|d| d.language == "csharp" && is_csharp_type_kind(&d.kind))
            .cloned()
            .collect();
        let bases = Self::cached_regex(&format!(r"(?-u:\b){}\s*:\s*(.*?)(?:(?-u:\b)where(?-u:\b)|;|$)", regex::escape(type_name)))?;
        let mut names = Vec::new();
        for decl in decls {
            let Some(lines) = self.read_file(&decl.file_path) else { continue };
            let from = ((decl.start_line - 1).max(0) as usize).min(lines.len());
            let to = ((decl.start_line + 8).max(0) as usize).min(lines.len());
            let joined = lines[from..to.max(from)].join(" ");
            let head = joined.split('{').next().unwrap_or("");
            // Blank nested `<…>` and `(…)` (type arguments, primary constructors).
            let mut depth = 0usize;
            let mut flat = String::new();
            for ch in head.chars() {
                match ch {
                    '<' | '(' => depth += 1,
                    '>' | ')' => depth = depth.saturating_sub(1),
                    _ if depth == 0 => flat.push(ch),
                    _ => {}
                }
            }
            if let Some(m) = bases.captures(&flat) {
                for t in re!(r"[A-Za-z_][A-Za-z0-9_.]*").find_iter(&m[1]) {
                    names.push(t.as_str().rsplit('.').next().unwrap_or("").to_string());
                }
            }
        }
        let names = Rc::new(names);
        self.csharp_supers_memo.insert(type_name.to_string(), names.clone());
        Ok(names)
    }

    /// csharpStaticUsings: the types a C# file sees through static usings:
    /// its own `using static A.B.Type;`, a `global using static` in any file of
    /// the same project (the nearest `.csproj` directory above each file), and
    /// `<Using Include="A.B.Type" Static="true"/>` in the `.csproj` /
    /// `Directory.Build.props` files above it.
    fn csharp_static_usings(&mut self, file: &str) -> Res<Rc<HashSet<String>>> {
        if let Some(hit) = self.csharp_static_usings_memo.get(file) {
            return Ok(hit.clone());
        }
        let dir = pos_dirname(file);
        let mut owners: HashSet<String> = self.csharp_project_static_usings(dir).as_ref().clone();
        let project = self.csharp_project_of(dir);
        if let Some(globals) = self.csharp_global_static_usings()?.get(&project) {
            owners.extend(globals.iter().cloned());
        }
        if let Some(src) = self.read_file(file) {
            for m in re!(r"(?m)^\s*(?:global\s+)?using\s+static\s+([A-Za-z0-9_.]+)\s*;").captures_iter(src.text()) {
                owners.insert(m[1].rsplit('.').next().unwrap_or("").to_string());
            }
        }
        let owners = Rc::new(owners);
        self.csharp_static_usings_memo.insert(file.to_string(), owners.clone());
        Ok(owners)
    }

    /// Every `global using static` in the index, grouped by the project of
    /// the file declaring it. Read once from the import nodes' directive text
    /// rather than from the source files.
    fn csharp_global_static_usings(&mut self) -> Res<Rc<HashMap<Option<String>, HashSet<String>>>> {
        if let Some(hit) = &self.csharp_global_statics {
            return Ok(hit.clone());
        }
        let rows: Vec<(String, String)> = {
            let mut stmt = self
                .conn()?
                .prepare(
                    "SELECT file_path, signature FROM nodes \
                     WHERE kind = 'import' AND language = 'csharp' AND signature LIKE 'global%' \
                     ORDER BY file_path, start_line",
                )
                .map_err(|e| Error::from_reason(e.to_string()))?;
            let mapped = stmt
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?.unwrap_or_default())))
                .map_err(|e| Error::from_reason(e.to_string()))?;
            mapped.filter_map(|row| row.ok()).collect()
        };
        let mut by_project: HashMap<Option<String>, HashSet<String>> = HashMap::new();
        for (file, signature) in rows {
            let Some(m) = re!(r"^\s*global\s+using\s+static\s+([A-Za-z0-9_.]+)\s*;").captures(&signature) else { continue };
            let owner = m[1].rsplit('.').next().unwrap_or("").to_string();
            let project = self.csharp_project_of(pos_dirname(&file));
            by_project.entry(project).or_default().insert(owner);
        }
        let by_project = Rc::new(by_project);
        self.csharp_global_statics = Some(by_project.clone());
        Ok(by_project)
    }

    /// The nearest directory at or above `dir` holding a `.csproj`; `None`
    /// when there is none, so project-less files share one scope.
    fn csharp_project_of(&mut self, dir: &str) -> Option<String> {
        if let Some(hit) = self.csharp_project_memo.get(dir) {
            return hit.clone();
        }
        let found = if self.csharp_dir_entries(dir).iter().any(|e| e.to_ascii_lowercase().ends_with(".csproj")) {
            Some(dir.to_string())
        } else if dir.is_empty() {
            None
        } else {
            self.csharp_project_of(pos_dirname(dir))
        };
        self.csharp_project_memo.insert(dir.to_string(), found.clone());
        found
    }

    /// The sorted entry names of a repository directory.
    fn csharp_dir_entries(&self, dir: &str) -> Vec<String> {
        let abs = if dir.is_empty() { self.root_abs.clone() } else { pos_resolve(&self.root_abs, dir) };
        let mut entries: Vec<String> = std::fs::read_dir(&abs)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        entries.sort();
        entries
    }

    /// The static usings the project files at or above `dir` declare.
    fn csharp_project_static_usings(&mut self, dir: &str) -> Rc<HashSet<String>> {
        let key = format!("dir:{dir}");
        if let Some(hit) = self.csharp_static_usings_memo.get(&key) {
            return hit.clone();
        }
        let mut owners: HashSet<String> = HashSet::new();
        if !dir.is_empty() {
            owners.extend(self.csharp_project_static_usings(pos_dirname(dir)).iter().cloned());
        }
        for entry in self.csharp_dir_entries(dir) {
            let lower = entry.to_ascii_lowercase();
            if !(lower.ends_with(".csproj") || lower.ends_with(".props")) {
                continue;
            }
            let rel = if dir.is_empty() { entry } else { format!("{dir}/{entry}") };
            let Some(text) = self.read_file(&rel) else { continue };
            for m in re!(r#"(?i)<Using\s+Include\s*=\s*"([A-Za-z0-9_.]+)"[^>]*(?-u:\b)Static\s*=\s*"true""#).captures_iter(text.text()) {
                owners.insert(m[1].rsplit('.').next().unwrap_or("").to_string());
            }
        }
        let owners = Rc::new(owners);
        self.csharp_static_usings_memo.insert(key, owners.clone());
        owners
    }

    // -- Objective-C ----------------------------------------------------------

    /// objcCallShape: how a bare Objective-C name is written on its line.
    fn objc_call_shape(&mut self, r: &ResolveRefIn) -> Option<ObjcShape> {
        let lines = self.read_file(&r.file_path)?;
        let line = lines.get((r.line - 1).max(0) as usize)?;
        let name = r.reference_name.split(':').next().unwrap_or("");
        if name.is_empty() {
            return None;
        }
        let mut shape = None;
        let column=names::js_unit_to_byte(line,r.column.max(0) as usize);
        let occurrences:Vec<_>=word_occurrences(line, name, true).collect();
        let at=occurrences.iter().copied().find(|at|*at>=column).or_else(||occurrences.last().copied())?;
        {
            let before = &line[..at];
            let after = &line[at + name.len()..];
            if after.trim_start().starts_with('(') && !re!(r"\[\s*[A-Za-z0-9_.]+\s+$").is_match(before) {
                shape.get_or_insert(ObjcShape::CCall);
            } else if re!(r"\[\s*super\s+$").is_match(before) {
                return Some(ObjcShape::SuperSend);
            } else if re!(r"\[\s*(?:self|\[\s*self\s+class\s*\])\s+$").is_match(before) {
                return Some(ObjcShape::SelfSend);
            }
        }
        shape
    }

    /// isObjcSelfSendTarget: a message to `self` / `super` means a method of
    /// a class in the sender's hierarchy — SDWebImage's `[self class]` went
    /// to SDWeakProxy's `class` 71 times. When tree-sitter-objc loses an
    /// `@implementation` its methods are indexed as functions: one in the
    /// sender's own file, or in the file named after a class of its
    /// hierarchy, still counts (as does any, when the sender's own class was
    /// lost too). A function elsewhere is never what `[super init]` sends to.
    fn is_objc_self_send_target(&mut self, n: &KNode, r: &ResolveRefIn, to_super: bool) -> Res<bool> {
        let hierarchy = self.objc_hierarchy_at(r)?;
        let sender = self.objc_sender_at(r)?;
        if let Some(sender)=&sender {
            let mut pending=VecDeque::new();
            if to_super {pending.extend(self.objc_supertypes_at(sender,r)?);} else {pending.push_back(sender.clone());}
            let mut seen=HashSet::new();
            let mut owners=HashSet::new();
            while let Some(owner)=pending.pop_front() {
                if seen.len()>=30 {return Ok(false);}
                if !seen.insert(owner.clone()) {continue;}
                let members=self.nodes_by_qualified_name(&format!("{owner}::{}",r.reference_name))?;
                if members.iter().any(|m|m.language=="objc" && is_objc_member_kind(&m.kind)) {
                    owners.insert(owner);
                } else {pending.extend(self.objc_supertypes_at(&owner,r)?);}
            }
            if !owners.is_empty() {return Ok(owners.len()==1 && owners.contains(n.qualified_name.rsplit_once("::").map(|(owner,_)|owner).unwrap_or("")));}

        }
        let sender=if to_super {sender} else {None};
        if is_objc_member_kind(&n.kind) {
            let Some(cut) = n.qualified_name.rfind("::") else { return Ok(true) };
            return Ok(hierarchy.is_none_or(|h| h.contains(&n.qualified_name[..cut])) && sender.as_deref() != Some(&n.qualified_name[..cut]));
        }
        let Some(hierarchy) = hierarchy else { return Ok(true) };
        if n.file_path == r.file_path {
            return Ok(!to_super);
        }
        let base = pos_basename(&n.file_path);
        let stem = base.rfind('.').map_or(base, |i| &base[..i]);
        Ok(hierarchy.contains(stem) && sender.as_deref() != Some(stem))
    }

    fn objc_sender_at(&mut self, r: &ResolveRefIn) -> Res<Option<String>> {
        let nodes = self.nodes_in_file(&r.file_path)?;
        Ok(nodes.iter().filter(|c| c.kind == "class" && c.start_line <= r.line && c.end_line >= r.line)
            .min_by_key(|c| c.end_line-c.start_line).map(|c| c.name.clone())
            .or_else(|| nodes.iter().filter(|n| n.kind == "method" && n.start_line <= r.line && n.end_line >= r.line)
                .min_by_key(|n| n.end_line-n.start_line).and_then(|n| n.qualified_name.rsplit_once("::").map(|(o,_)| o.to_string()))))
    }

    /// objcHierarchyAt: the class a message is written in and every class it
    /// inherits from; `None` outside any class.
    fn objc_hierarchy_at(&mut self, r: &ResolveRefIn) -> Res<Option<Rc<HashSet<String>>>> {
        let here = self.objc_sender_at(r)?;
        let Some(here) = here else { return Ok(None) };
        let key=format!("{}\0{here}",r.file_path);
        if let Some(hit) = self.objc_hierarchy_memo.get(&key) {
            return Ok(Some(hit.clone()));
        }
        let mut seen=HashSet::new();let mut pending=VecDeque::from([here]);
        while seen.len()<30 {
            let Some(name)=pending.pop_front() else {break};
            if !seen.insert(name.clone()) {continue;}
            pending.extend(self.objc_supertypes_at(&name,r)?);
        }
        let seen=Rc::new(seen);
        self.objc_hierarchy_memo.insert(key, seen.clone());
        Ok(Some(seen))
    }

    fn objc_supertypes_at(&mut self,name:&str,r:&ResolveRefIn)->Res<Vec<String>> {
        let parents=self.objc_supertypes_of(name)?;
        let declarations:Vec<_>=self.nodes_by_name(name)?.iter().filter(|n|n.language=="objc" && matches!(n.kind.as_str(),"class"|"protocol")).cloned().collect();
        let pattern=Self::cached_regex(&format!(r"@interface\s+{}\s*:\s*([A-Za-z0-9_]+)",regex::escape(name)))?;
        let mut result=HashSet::new();
        for parent in parents.iter() {
            let mut declared=false;
            for decl in &declarations {
                let Some(lines)=self.read_file(&decl.file_path) else {continue};
                for (index,line) in lines.iter().enumerate() {
                    if pattern.captures(line).is_some_and(|m|&m[1]==parent) {
                        declared=true;
                        result.extend(self.cpp_type_alias_names(r,&decl.file_path,index as i64+1,parent));
                    }
                }
            }
            if !declared {result.insert(parent.clone());}
        }
        Ok(result.into_iter().collect())
    }

    /// Every class `start` reaches through `@interface` superclasses (at most `cap`).
    fn objc_type_closure(&mut self, start: Vec<String>, cap: usize) -> Res<HashSet<String>> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = start.into();
        while seen.len() < cap {
            let Some(name) = queue.pop_front() else { break };
            if !seen.insert(name.clone()) {
                continue;
            }
            queue.extend(self.objc_supertypes_of(&name)?.iter().cloned());
        }
        Ok(seen)
    }

    /// objcSupertypesOf: the superclasses an Objective-C class's `@interface`
    /// declarations name, or its UIKit/AppKit superclass.
    fn objc_supertypes_of(&mut self, name: &str) -> Res<Rc<Vec<String>>> {
        if let Some(hit) = self.objc_supers_memo.get(name) {
            return Ok(hit.clone());
        }
        let decls: Vec<Arc<KNode>> = self
            .nodes_by_name(name)?
            .iter()
            .filter(|d| matches!(d.kind.as_str(),"class"|"protocol") && d.language == "objc")
            .cloned()
            .collect();
        let mut supers: Vec<String> = Vec::new();
        for decl in decls {
            let Some(lines) = self.read_file(&decl.file_path) else { continue };
            let pattern=Self::cached_regex(&format!(r"@interface\s+{}\s*:\s*([A-Za-z0-9_]+)",regex::escape(name)))?;
            for line in lines.iter() {
                if let Some(m)=pattern.captures(line) {
                    if !supers.iter().any(|s|s==&m[1]) {supers.push(m[1].to_string());}
                }
            }
        }
        if supers.is_empty() {
            if let Some((_, sup)) = OBJC_SYSTEM_SUPERS.iter().find(|(c, _)| *c == name) {
                supers.push(sup.to_string());
            }
        }
        let supers = Rc::new(supers);
        self.objc_supers_memo.insert(name.to_string(), supers.clone());
        Ok(supers)
    }

    /// objcReceiverReaches: whether an Objective-C receiver's type has
    /// `method`'s owner in its hierarchy — the receiver names a class
    /// (`[AllTypesObject objectsInRealm:…]`, a class method inherited from
    /// RLMObject), or is a property whose `@property … Type *name`
    /// declarations give such a type (`managed.anyDataObj` → RLMSet).
    pub(super) fn objc_receiver_reaches(&mut self, receiver: &str, method: &KNode) -> Res<bool> {
        let Some(cut) = method.qualified_name.rfind("::") else { return Ok(false) };
        let owner = method.qualified_name[..cut].to_string();
        let types: Vec<String> = if re!(r"^[A-Z][A-Za-z0-9_]*$").is_match(receiver) {
            let is_class = self.nodes_by_name(receiver)?.iter().any(|n| n.kind == "class" && n.language == "objc");
            if is_class { vec![receiver.to_string()] } else { Vec::new() }
        } else if let Some((_, prop)) = receiver.rsplit_once('.') {
            self.objc_property_types(prop)?
        } else {
            Vec::new()
        };
        if types.is_empty() {
            return Ok(false);
        }
        Ok(self.objc_type_closure(types, 40)?.contains(&owner))
    }

    /// The classes the `@property … Type *name` declarations of `name` give.
    fn objc_property_types(&mut self, name: &str) -> Res<Vec<String>> {
        let decl = Self::cached_regex(&format!(
            r"@property\s*(?:\([^)]*\)\s*)?([A-Z][A-Za-z0-9_]*)\s*(?:<[^;]*>\s*)?\*\s*(?:_Nullable\s+|_Nonnull\s+)?{}(?-u:\b)",
            regex::escape(name)
        ))?;
        let props: Vec<Arc<KNode>> = self
            .nodes_by_name(name)?
            .iter()
            .filter(|n| n.kind == "property" && n.language == "objc")
            .cloned()
            .collect();
        let mut types: Vec<String> = Vec::new();
        for p in props {
            let Some(lines) = self.read_file(&p.file_path) else { continue };
            let Some(line) = lines.get((p.start_line - 1).max(0) as usize) else { continue };
            if let Some(m) = decl.captures(line) {
                if !types.iter().any(|t| t == &m[1]) {
                    types.push(m[1].to_string());
                }
            }
        }
        Ok(types)
    }
}

/// isVbMemberReachable: a VB.NET member access can mean `n` through `Me` /
/// `MyBase` / `MyClass`, or through a name that is `n`'s own type or module
/// (`Module1.Log()`, `Colors.Red`). Any other receiver has a type nothing
/// here names: SCrawler's `New System.Drawing.Size(…)` went to a nested
/// enum's `Size` case 713 times, its designer's `Controls.Add(…)` to a
/// collection class's `Add` 547.
fn is_vb_member_reachable(n: &KNode, receiver: &str) -> bool {
    if !is_vb_member_kind(&n.kind) {
        return true;
    }
    let Some(owner) = owner_simple_name(n) else { return true };
    let last = receiver.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if !receiver.contains('.') && matches!(last.as_str(), "me" | "mybase" | "myclass") {
        return true;
    }
    !last.is_empty() && last == owner.to_ascii_lowercase()
}
