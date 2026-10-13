//! Direct port of src/resolution/frameworks/nestjs.ts, retaining its regex heuristics.
use crate::text::{capture, first, js_len, js_trim, line_at, matches, strip_comments, test};
use crate::types::{Node, ResolutionContext, ResolvedRef, UnresolvedRef};
use indexmap::{IndexMap, IndexSet};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const NAME: &str = "nestjs";
pub const LANGUAGES: &[&str] = &["typescript", "javascript"];

const HTTP: &[&str] = &[
    "Get", "Post", "Put", "Patch", "Delete", "Head", "Options", "All",
];
const GQL: &[&str] = &["Query", "Mutation", "Subscription"];
const HTTP_QN: &str = r"::(?:GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|ALL):";
const NEUTRAL: &str = "\0neutral";
const CONVENTIONS: &[(&str, &str)] = &[
    ("Service", ".service."),
    ("Controller", ".controller."),
    ("Resolver", ".resolver."),
    ("Gateway", ".gateway."),
    ("Repository", ".repository."),
    ("Guard", ".guard."),
    ("Interceptor", ".interceptor."),
    ("Pipe", ".pipe."),
    ("Module", ".module."),
];

pub fn declared_dependencies(context: &impl ResolutionContext) -> IndexSet<String> {
    let files = context.all_files();
    if let Some((count, names)) = context.dependency_cache().borrow().as_ref()
        && *count == files.len()
    {
        return names.clone();
    }

    let mut dirs = IndexSet::new();
    for file in &files {
        let segs: Vec<_> = file.split('/').collect();
        if segs.len() > 1 {
            dirs.insert(format!("{}/", segs[0]));
        }
        if segs.len() > 2 {
            dirs.insert(format!("{}/{}/", segs[0], segs[1]));
        }
        if dirs.len() > 192 {
            break;
        }
    }
    let mut manifests = vec!["package.json".to_owned()];
    for dir in dirs {
        if manifests.len() > 24 {
            break;
        }
        let manifest = format!("{dir}package.json");
        if !dir.contains("node_modules") && context.file_exists(&manifest) {
            manifests.push(manifest);
        }
    }
    if manifests.len() <= 24 {
        for dir in deep_script_directories(&files) {
            if manifests.len() > 24 {
                break;
            }
            let manifest = format!("{dir}package.json");
            if context.file_exists(&manifest) {
                manifests.push(manifest);
            }
        }
    }
    let mut names = IndexSet::new();
    for manifest in manifests {
        let Some(content) = context.read_file(&manifest) else {
            continue;
        };
        let Ok(pkg) = serde_json::from_str::<Value>(content) else {
            continue;
        };
        for group in ["dependencies", "devDependencies", "peerDependencies"] {
            if let Some(obj) = pkg[group].as_object() {
                names.extend(obj.keys().cloned());
            }
            // Object.keys also accepts an array in a malformed dependency group.
            if let Some(arr) = pkg[group].as_array() {
                names.extend((0..arr.len()).map(|i| i.to_string()));
            }
        }
    }
    *context.dependency_cache().borrow_mut() = Some((files.len(), names.clone()));
    names
}
fn deep_script_directories(files: &[&str]) -> Vec<String> {
    let mut levels: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for file in files {
        if !test(r"\.(?:[cm]?[jt]sx?|vue|svelte|astro)$", file) || file.contains("node_modules") {
            continue;
        }
        for (index, (slash, _)) in file.match_indices('/').enumerate() {
            if index >= 2 {
                levels
                    .entry(index)
                    .or_default()
                    .insert(file[..=slash].to_owned());
            }
        }
    }
    levels
        .into_values()
        .flat_map(|level| {
            let mut dirs: Vec<_> = level.into_iter().collect();
            dirs.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            dirs
        })
        .take(192)
        .collect()
}
pub fn detect(context: &impl ResolutionContext) -> bool {
    if declared_dependencies(context)
        .iter()
        .any(|n| n.starts_with("@nestjs/"))
    {
        return true;
    }
    for file in context.all_files() {
        if [
            ".controller.ts",
            ".controller.js",
            ".module.ts",
            ".resolver.ts",
            ".gateway.ts",
        ]
        .iter()
        .any(|s| file.ends_with(s))
            && context.read_file(file).is_some_and(|s| {
                [
                    "@nestjs/",
                    "@Controller",
                    "@Module(",
                    "@Resolver(",
                    "@WebSocketGateway(",
                ]
                .iter()
                .any(|p| s.contains(p))
            })
        {
            return true;
        }
    }
    false
}
pub fn resolve(reference: &UnresolvedRef, context: &impl ResolutionContext) -> Option<ResolvedRef> {
    for (suffix, convention) in CONVENTIONS {
        if !reference.reference_name.ends_with(suffix) {
            continue;
        }
        let candidates: Vec<_> = context
            .nodes_by_name(&reference.reference_name)
            .into_iter()
            .filter(|n| n.kind == "class")
            .collect();
        let own = candidates
            .iter()
            .find(|n| n.file_path == reference.file_path);
        let (target, confidence) = if let Some(own) = own {
            (Some(*own), 0.85)
        } else if test(r"\.(?:m?[jt]sx?|cjs|cts|mts)$", &reference.file_path) {
            let dir = &reference.file_path[..reference.file_path.rfind('/').map_or(0, |i| i + 1)];
            (
                candidates
                    .iter()
                    .find(|n| {
                        n.file_path.contains(convention)
                            && n.file_path.starts_with(dir)
                            && !n.file_path[dir.len()..].contains('/')
                    })
                    .copied(),
                0.85,
            )
        } else {
            let preferred = candidates
                .iter()
                .find(|n| n.file_path.contains(convention))
                .copied();
            (
                preferred.or_else(|| candidates.first().copied()),
                if preferred.is_some() { 0.85 } else { 0.7 },
            )
        };
        return target.map(|n| ResolvedRef {
            original: reference.clone(),
            target_node_id: n.id.clone(),
            confidence,
            resolved_by: "framework",
        });
    }
    None
}
#[derive(Serialize, Default)]
pub struct Extraction {
    pub nodes: Vec<Node>,
    pub references: Vec<UnresolvedRef>,
}
pub fn extract(file_path: &str, content: &str, now: i64) -> Extraction {
    let mut output = Extraction::default();
    if !test(r"\.(m?js|tsx?|cjs)$", file_path) {
        return output;
    }
    let lang = detect_language(file_path);
    let safe = strip_comments(content);
    let scopes = build_class_scopes(&safe);
    let mut add_route =
        |hit: &DecoratorHit, method: &str, path: String, handler: Option<String>| {
            let line = line_at(&safe, hit.index);
            let id = format!("route:{file_path}:{line}:{method}:{path}");
            output.nodes.push(Node {
                id: id.clone(),
                kind: "route".into(),
                name: format!("{method} {path}"),
                qualified_name: format!("{file_path}::{method}:{path}"),
                file_path: file_path.into(),
                start_line: line,
                end_line: line,
                start_column: 0,
                end_column: js_len(&safe[hit.index..hit.end]),
                language: lang.into(),
                updated_at: now,
                extra: Map::new(),
            });
            if let Some(handler) = handler.filter(|h| !h.is_empty()) {
                output.references.push(UnresolvedRef {
                    from_node_id: id,
                    reference_name: handler,
                    reference_kind: "references".into(),
                    line,
                    column: 0,
                    file_path: file_path.into(),
                    language: lang.into(),
                    extra: Map::new(),
                });
            }
        };
    for hit in find_decorators(&safe, HTTP) {
        let prefix = scope_for(&scopes, hit.index)
            .filter(|s| s.kind == "controller")
            .map_or("", |s| s.prefix.as_str());
        add_route(
            &hit,
            &hit.name.to_uppercase(),
            join_http_path(prefix, &parse_string_arg(&hit.args)),
            method_name_after(&safe, hit.end),
        );
    }
    for hit in find_decorators(&safe, GQL) {
        if !scope_for(&scopes, hit.index).is_some_and(|s| s.kind == "resolver") {
            continue;
        }
        let handler = method_name_after(&safe, hit.end);
        add_route(
            &hit,
            &hit.name.to_uppercase(),
            parse_graphql_name(&hit.args, handler.as_deref()),
            handler,
        );
    }
    for hit in find_decorators(&safe, &["MessagePattern", "EventPattern"]) {
        let handler = method_name_after(&safe, hit.end);
        let arg = parse_string_arg(&hit.args);
        let path = if arg.is_empty() {
            handler.clone().unwrap_or_default()
        } else {
            arg
        };
        add_route(
            &hit,
            if hit.name == "EventPattern" {
                "EVENT"
            } else {
                "MESSAGE"
            },
            path,
            handler,
        );
    }
    for hit in find_decorators(&safe, &["SubscribeMessage"]) {
        let namespace = scope_for(&scopes, hit.index)
            .filter(|s| s.kind == "gateway")
            .map_or("", |s| s.prefix.as_str());
        let handler = method_name_after(&safe, hit.end);
        let arg = parse_string_arg(&hit.args);
        let event = if arg.is_empty() {
            handler.clone().unwrap_or_default()
        } else {
            arg
        };
        let path = if namespace.is_empty() {
            event
        } else {
            format!("{namespace}:{event}")
        };
        add_route(&hit, "WS", path, handler);
    }
    output
}

#[derive(Default, Clone)]
struct AppConfig {
    prefix: String,
    excludes: Vec<String>,
    uri: bool,
    default_version: Option<String>,
    version_prefix: String,
}
struct Bootstrap {
    root: String,
    config: AppConfig,
}
struct Planned<'a> {
    route: &'a Node,
    module_prefix: String,
    config: Option<&'a AppConfig>,
    // Outer None is JS undefined; Some(None) is an explicitly unparsed version.
    version: Option<Option<String>>,
}
pub fn post_extract(context: &impl ResolutionContext) -> Vec<Node> {
    let mut module_to_prefix = IndexMap::new();
    let mut controller_to_module = IndexMap::new();
    let mut bootstraps = Vec::new();
    for file_path in context.all_files() {
        if !test(r"\.(m?[jt]s|cjs)$", file_path) {
            continue;
        }
        let is_module = test(r"\.module\.(m?[jt]s|cjs)$", file_path);
        if !is_module && !context.file_contains(file_path, "NestFactory") {
            continue;
        }
        let Some(content) = context.read_file(file_path).filter(|s| !s.is_empty()) else {
            continue;
        };
        let safe = strip_comments(content);
        if is_module {
            collect_router_module_registrations(&safe, &mut module_to_prefix);
            collect_module_controllers(&safe, &mut controller_to_module);
        }
        if test(r"\bNestFactory\s*\.\s*create", &safe) {
            bootstraps.push(Bootstrap {
                root: app_root_of(file_path),
                config: read_bootstrap(&safe),
            });
        }
    }
    let controller_to_prefix: IndexMap<_, _> = controller_to_module
        .iter()
        .filter_map(|(controller, module)| {
            module_to_prefix
                .get(module)
                .filter(|p| !p.is_empty() && *p != "/")
                .map(|p| (controller, p))
        })
        .collect();
    let adds = bootstraps
        .iter()
        .any(|b| !b.config.prefix.is_empty() || b.config.uri);
    if controller_to_prefix.is_empty() && !adds {
        return vec![];
    }
    let mut plan: IndexMap<String, Planned<'_>> = IndexMap::new();
    for (controller_name, prefix) in controller_to_prefix {
        for cls in context
            .nodes_by_name(controller_name)
            .into_iter()
            .filter(|n| n.kind == "class")
        {
            for route in context.nodes_in_file(&cls.file_path) {
                if route.kind != "route"
                    || route.start_line < cls.start_line
                    || route.start_line > cls.end_line
                {
                    continue;
                }
                plan.insert(
                    route.id.clone(),
                    Planned {
                        route,
                        module_prefix: prefix.clone(),
                        config: None,
                        version: None,
                    },
                );
            }
        }
    }
    if adds {
        for file_path in context.all_files() {
            if !test(r"\.(m?[jt]s|cjs)$", file_path)
                || !context.file_contains(file_path, "@Controller")
            {
                continue;
            }
            let Some(config) = bootstrap_for(&bootstraps, file_path) else {
                continue;
            };
            if config.prefix.is_empty() && !config.uri {
                continue;
            }
            let routes: Vec<_> = context
                .nodes_in_file(file_path)
                .into_iter()
                .filter(|n| n.kind == "route" && test(HTTP_QN, &n.qualified_name))
                .collect();
            if routes.is_empty() {
                continue;
            }
            let Some(content) = context.read_file(file_path).filter(|s| !s.is_empty()) else {
                continue;
            };
            let safe = strip_comments(content);
            let scopes = build_class_scopes(&safe);
            let hits = find_decorators(&safe, HTTP);
            for route in routes {
                let Some(hit) = hits
                    .iter()
                    .find(|h| line_at(&safe, h.index) == route.start_line)
                else {
                    continue;
                };
                let Some(scope) = scope_for(&scopes, hit.index).filter(|s| s.kind == "controller")
                else {
                    continue;
                };
                let module_prefix = plan
                    .get(&route.id)
                    .map(|p| p.module_prefix.clone())
                    .unwrap_or_default();
                plan.insert(
                    route.id.clone(),
                    Planned {
                        route,
                        module_prefix,
                        config: Some(config),
                        version: route_version(&safe, hit, scope),
                    },
                );
            }
        }
    }
    plan.values()
        .filter_map(|p| {
            apply_app_config(
                p.route,
                &p.module_prefix,
                p.config,
                p.version.as_ref(),
                context.now(),
            )
        })
        .filter(|n| {
            context
                .nodes_in_file(&n.file_path)
                .iter()
                .find(|r| r.id == n.id)
                .is_some_and(|r| r.name != n.name)
        })
        .collect()
}
fn app_root_of(file_path: &str) -> String {
    capture(r"^((?:[^/]+/)*?)src/", file_path, 1).unwrap_or_default()
}
fn bootstrap_for<'a>(bootstraps: &'a [Bootstrap], file_path: &str) -> Option<&'a AppConfig> {
    let mut root: Option<&str> = None;
    for b in bootstraps {
        if file_path.starts_with(&b.root) && root.is_none_or(|r| b.root.len() > r.len()) {
            root = Some(&b.root);
        }
    }
    if root.is_none() {
        if bootstraps
            .iter()
            .map(|b| &b.root)
            .collect::<BTreeSet<_>>()
            .len()
            != 1
        {
            return None;
        }
        root = Some(&bootstraps[0].root);
    }
    let mut here = bootstraps.iter().filter(|b| Some(b.root.as_str()) == root);
    here.clone()
        .find(|b| !b.config.prefix.is_empty() || b.config.uri)
        .or_else(|| here.next())
        .map(|b| &b.config)
}
fn version_value(text: &str) -> Option<String> {
    let t = js_trim(text);
    if test(r"^VERSION_NEUTRAL\b", t) {
        return Some(NEUTRAL.into());
    }
    capture(r#"^\[?\s*(['"`])([^'"`]*)\1"#, t, 2)
}
fn call_args<'a>(safe: &'a str, pattern: &str) -> Option<&'a str> {
    let m = first(pattern, safe)?;
    let open = m.end - 1;
    Some(
        matching_close(safe, open)
            .map(|close| &safe[open + 1..close])
            .unwrap_or(""),
    )
}
fn read_bootstrap(safe: &str) -> AppConfig {
    let mut config = AppConfig {
        version_prefix: "v".into(),
        ..AppConfig::default()
    };
    if let Some(args) = call_args(safe, r"\.setGlobalPrefix\s*\(")
        && let Some(lit) = capture(r#"^\s*(['"`])([^'"`$]*)\1"#, args, 2)
    {
        config.prefix = lit.trim_matches('/').into();
        if let Some(ex) = first(r"\bexclude\s*:\s*\[", args) {
            let open = ex.end - 1;
            let list = matching_close(args, open)
                .map(|close| &args[open + 1..close])
                .unwrap_or("");
            for item in split_top_level(list) {
                let text = js_trim(item);
                if let Some(path) = capture(r#"^(['"])([^'"]*)\1$"#, text, 2)
                    .or_else(|| capture(r#"\bpath\s*:\s*(['"])([^'"]*)\1"#, text, 2))
                {
                    config.excludes.push(path.trim_start_matches('/').into());
                }
            }
        }
    }
    if let Some(args) = call_args(safe, r"\.enableVersioning\s*\(") {
        let kind = capture(r"\btype\s*:\s*VersioningType\s*\.\s*(\w+)", args, 1);
        config.uri = kind.is_none_or(|t| t == "URI");
        if let Some(def) = first(r"\bdefaultVersion\s*:\s*", args) {
            config.default_version = version_value(&args[def.end..]);
        }
        if let Some(pre) = first(r#"\bprefix\s*:\s*(false|(['"`])([^'"`]*)\2)"#, args) {
            config.version_prefix = if pre.group(1) == "false" {
                String::new()
            } else {
                pre.group(3).into()
            };
        }
    }
    config
}
fn is_excluded(path: &str, excludes: &[String]) -> bool {
    let route = path.trim_start_matches('/');
    excludes.iter().any(|pattern| {
        let wild = first(r"(?:\{/?\*\w*\}|/?\(\.\*\)|/?\*\w*)$", pattern);
        let base =
            pattern[..wild.as_ref().map_or(pattern.len(), |m| m.start)].trim_end_matches('/');
        route == base || wild.is_some() && route.starts_with(&format!("{base}/"))
    })
}
fn route_version(safe: &str, hit: &DecoratorHit, scope: &ClassScope) -> Option<Option<String>> {
    let from = decorator_chain_start(safe, hit.index, scope.start);
    let handler = method_name_after(safe, hit.end);
    let at = handler.and_then(|h| {
        first(
            &format!(r"(?<![@\w$]){}\s*\(", h.replace('$', r"\$")),
            &safe[hit.end..],
        )
    });
    let to = at.map_or(hit.end, |m| hit.end + m.start);
    if let Some(own) = capture(r"@Version\s*\(([^)]*)\)", &safe[from..to], 1) {
        return Some(version_value(&own));
    }
    let end = first(r"\bclass\b", &safe[scope.start..]).map_or_else(
        || {
            safe[scope.start..]
                .char_indices()
                .scan(0, |units, (at, c)| {
                    *units += c.len_utf16();
                    Some((at, *units))
                })
                .find(|(_, n)| *n > 400)
                .map_or(safe.len(), |(at, _)| scope.start + at)
        },
        |m| scope.start + m.start,
    );
    let head = &safe[decorator_chain_start(safe, scope.start, 0)..end];
    if let Some(on_class) = capture(r"@Version\s*\(([^)]*)\)", head, 1) {
        return Some(version_value(&on_class));
    }
    capture(
        r"@Controller\s*\(\s*\{[^)]*\bversion\s*:\s*([^,}]+)",
        head,
        1,
    )
    .map(|s| version_value(&s))
}
fn decorator_chain_start(safe: &str, at: usize, floor: usize) -> usize {
    let b = safe.as_bytes();
    let mut start = at;
    loop {
        let mut i = start;
        while i > floor {
            let c = safe[..i].chars().next_back().unwrap();
            if !js_trim(&c.to_string()).is_empty() {
                break;
            }
            i -= c.len_utf8();
        }
        if i > 0 && b[i - 1] == b')' {
            let mut depth = 0;
            let mut found = None;
            for j in (floor..i).rev() {
                if b[j] == b')' {
                    depth += 1;
                } else if b[j] == b'(' {
                    depth -= 1;
                    if depth == 0 {
                        found = Some(j);
                        break;
                    }
                }
            }
            let Some(j) = found else {
                return start;
            };
            i = j;
        }
        let mut name = i;
        while name > floor
            && (b[name - 1].is_ascii_alphanumeric()
                || b"_$ .".contains(&b[name - 1]) && b[name - 1] != b' ')
        {
            name -= 1;
        }
        if name == i || name == 0 || b[name - 1] != b'@' {
            return start;
        }
        start = name - 1;
    }
}
fn apply_app_config(
    route: &Node,
    module_prefix: &str,
    config: Option<&AppConfig>,
    version: Option<&Option<String>>,
    now: i64,
) -> Option<Node> {
    let (_, tail) = route.qualified_name.split_once("::")?;
    let (method, original) = tail.split_once(':')?;
    let inner = join_http_path(module_prefix, original);
    let path = if let Some(config) = config {
        let v = version.unwrap_or(&config.default_version);
        let version_seg = v
            .as_ref()
            .filter(|v| config.uri && v.as_str() != NEUTRAL)
            .map(|v| format!("{}{v}", config.version_prefix))
            .unwrap_or_default();
        let prefix = if !config.prefix.is_empty() && !is_excluded(&inner, &config.excludes) {
            &config.prefix
        } else {
            ""
        };
        join_http_path(&join_http_path(prefix, &version_seg), &inner)
    } else {
        inner
    };
    let mut updated = route.clone();
    updated.name = format!("{method} {path}");
    updated.updated_at = now;
    Some(updated)
}

#[derive(Debug)]
struct DecoratorHit {
    name: String,
    args: String,
    index: usize,
    end: usize,
}
fn find_decorators(safe: &str, names: &[&str]) -> Vec<DecoratorHit> {
    let mut hits = vec![];
    let mut after = 0;
    for m in matches(&format!(r"@({})\s*\(", names.join("|")), safe) {
        if m.start < after {
            continue;
        }
        if let Some((args, end)) = read_args(safe, m.end - 1) {
            hits.push(DecoratorHit {
                name: m.group(1).into(),
                args: args.into(),
                index: m.start,
                end,
            });
            after = end;
        }
    }
    hits
}
fn read_args(s: &str, open: usize) -> Option<(&str, usize)> {
    if s.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    balanced_close(s, open, false).map(|close| (&s[open + 1..close], close + 1))
}
fn skip_decorators(safe: &str, start: usize) -> Option<usize> {
    let mut i = start;
    loop {
        i += first(r"^\s*", &safe[i..]).unwrap().end;
        if safe.as_bytes().get(i) != Some(&b'@') {
            break;
        }
        let Some(m) = first(r"^@[\w.]+", &safe[i..]) else {
            break;
        };
        i += m.end;
        i += first(r"^\s*", &safe[i..]).unwrap().end;
        if safe.as_bytes().get(i) == Some(&b'(') {
            i = read_args(safe, i)?.1;
        }
    }
    Some(i)
}
fn method_name_after(safe: &str, start: usize) -> Option<String> {
    let mut i = skip_decorators(safe, start)?;
    loop {
        i += first(r"^\s*", &safe[i..]).unwrap().end;
        if let Some(m) = first(r"^(?:public|private|protected|async|static)\b", &safe[i..]) {
            i += m.end;
        } else {
            break;
        }
    }
    capture(r"^([A-Za-z_$][\w$]*)\s*\(", &safe[i..], 1)
}
fn class_name_after(safe: &str, start: usize) -> Option<String> {
    let i = skip_decorators(safe, start)?;
    capture(
        r"^(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)",
        &safe[i..],
        1,
    )
}
struct ClassScope {
    kind: &'static str,
    prefix: String,
    start: usize,
    end: usize,
}
fn build_class_scopes(safe: &str) -> Vec<ClassScope> {
    let mut raw = vec![];
    for (kind, name) in [
        ("controller", "Controller"),
        ("resolver", "Resolver"),
        ("gateway", "WebSocketGateway"),
        ("other", "Injectable"),
        ("other", "Module"),
        ("other", "Catch"),
    ] {
        for hit in find_decorators(safe, &[name]) {
            let prefix = match kind {
                "controller" => capture(r#"path\s*:\s*['"`]([^'"`]*)['"`]"#, &hit.args, 1)
                    .unwrap_or_else(|| parse_string_arg(&hit.args)),
                "gateway" => capture(r#"namespace\s*:\s*['"`]([^'"`]*)['"`]"#, &hit.args, 1)
                    .unwrap_or_default(),
                _ => String::new(),
            };
            raw.push(ClassScope {
                kind,
                prefix,
                start: hit.index,
                end: safe.len(),
            });
        }
    }
    raw.sort_by_key(|r| r.start);
    for i in 0..raw.len().saturating_sub(1) {
        raw[i].end = raw[i + 1].start;
    }
    raw
}
fn scope_for(scopes: &[ClassScope], index: usize) -> Option<&ClassScope> {
    scopes.iter().find(|s| index >= s.start && index < s.end)
}
fn parse_string_arg(args: &str) -> String {
    capture(r#"['"`]([^'"`]*)['"`]"#, args, 1).unwrap_or_default()
}
fn parse_graphql_name(args: &str, handler: Option<&str>) -> String {
    capture(r#"name\s*:\s*['"`]([^'"`]*)['"`]"#, args, 1)
        .or_else(|| capture(r#"^\s*['"`]([^'"`]*)['"`]"#, args, 1))
        .unwrap_or_else(|| handler.unwrap_or("").into())
}
fn join_http_path(prefix: &str, sub: &str) -> String {
    format!(
        "/{}",
        [prefix, sub]
            .iter()
            .map(|p| js_trim(p).trim_matches('/'))
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    )
}
fn detect_language(path: &str) -> &'static str {
    if path.ends_with(".ts") || path.ends_with(".tsx") {
        "typescript"
    } else {
        "javascript"
    }
}

fn collect_router_module_registrations(safe: &str, out: &mut IndexMap<String, String>) {
    let mut after = 0;
    for m in matches(
        r"\bRouterModule\s*\.\s*(?:register|forRoot|forChild)\s*\(",
        safe,
    ) {
        if m.start < after {
            continue;
        }
        if let Some((args, end)) = read_args(safe, m.end - 1) {
            walk_routes_tree(&parse_routes_array(args), "", out);
            after = end;
        }
    }
}
struct RouteItem {
    path: String,
    module_name: Option<String>,
    children: Vec<RouteItem>,
}
fn parse_routes_array(args: &str) -> Vec<RouteItem> {
    let trimmed = js_trim(args);
    if !trimmed.starts_with('[') {
        return vec![];
    }
    matching_close(trimmed, 0)
        .map(|close| parse_route_objects(&trimmed[1..close]))
        .unwrap_or_default()
}
fn parse_route_objects(s: &str) -> Vec<RouteItem> {
    split_top_level_objects(s)
        .iter()
        .map(|obj| RouteItem {
            path: parse_string_field(obj, "path"),
            module_name: parse_ident_field(obj, "module"),
            children: parse_array_field(obj, "children")
                .map(parse_route_objects)
                .unwrap_or_default(),
        })
        .collect()
}
fn walk_routes_tree(items: &[RouteItem], parent: &str, out: &mut IndexMap<String, String>) {
    for item in items {
        let prefix = join_http_path(parent, &item.path);
        if let Some(name) = &item.module_name {
            out.entry(name.clone()).or_insert_with(|| prefix.clone());
        }
        walk_routes_tree(&item.children, &prefix, out);
    }
}
fn collect_module_controllers(safe: &str, out: &mut IndexMap<String, String>) {
    for hit in find_decorators(safe, &["Module"]) {
        let Some(name) = class_name_after(safe, hit.end) else {
            continue;
        };
        if let Some(inner) = parse_array_field(&hit.args, "controllers") {
            for controller in inner
                .split(',')
                .map(js_trim)
                .filter(|s| test(r"^[A-Za-z_$][\w$]*$", s))
            {
                out.entry(controller.into()).or_insert_with(|| name.clone());
            }
        }
    }
}
fn split_top_level(list: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut depth = 0;
    let mut quote = 0;
    let mut i = 0;
    let mut start = 0;
    let b = list.as_bytes();
    while i < b.len() {
        let ch = b[i];
        if quote != 0 {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == quote {
                quote = 0;
            }
        } else if b"\"'`".contains(&ch) {
            quote = ch;
        } else if b"[{(".contains(&ch) {
            depth += 1;
        } else if b"]})".contains(&ch) {
            depth -= 1;
        } else if ch == b',' && depth == 0 {
            out.push(&list[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    out.push(&list[start..]);
    out.into_iter().filter(|s| !js_trim(s).is_empty()).collect()
}
fn matching_close(s: &str, open: usize) -> Option<usize> {
    balanced_close(s, open, true)
}
fn balanced_close(s: &str, open: usize, all_brackets: bool) -> Option<usize> {
    let b = s.as_bytes();
    if !b.get(open).is_some_and(|c| b"[{(".contains(c)) {
        return None;
    }
    let mut depth = 0;
    let mut quote = 0;
    let mut i = open;
    while i < b.len() {
        let ch = b[i];
        if quote != 0 {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == quote {
                quote = 0;
            }
        } else if b"\"'`".contains(&ch) {
            quote = ch;
        } else if ch == b'(' || all_brackets && b"[{".contains(&ch) {
            depth += 1;
        } else if ch == b')' || all_brackets && b"]}".contains(&ch) {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}
fn split_top_level_objects(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut depth = 0;
    let mut start = None;
    let mut quote = 0;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let ch = b[i];
        if quote != 0 {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == quote {
                quote = 0;
            }
        } else if b"\"'`".contains(&ch) {
            quote = ch;
        } else if depth == 0 && ch == b'{' {
            depth = 1;
            start = Some(i);
        } else if b"{[(".contains(&ch) {
            depth += 1;
        } else if b"}])".contains(&ch) {
            depth -= 1;
            if depth == 0
                && ch == b'}'
                && let Some(at) = start.take()
            {
                out.push(&s[at + 1..i]);
            }
        }
        i += 1;
    }
    out
}
fn parse_string_field(obj: &str, name: &str) -> String {
    capture(
        &format!(r#"(?:^|[,{{\s]){name}\s*:\s*['"`]([^'"`]*)['"`]"#),
        obj,
        1,
    )
    .unwrap_or_default()
}
fn parse_ident_field(obj: &str, name: &str) -> Option<String> {
    capture(
        &format!(r"(?:^|[,{{\s]){name}\s*:\s*([A-Za-z_$][\w$]*)"),
        obj,
        1,
    )
}
fn parse_array_field<'a>(obj: &'a str, name: &str) -> Option<&'a str> {
    let m = first(&format!(r"(?:^|[,{{\s]){name}\s*:\s*\["), obj)?;
    let open = m.end - 1;
    matching_close(obj, open).map(|close| &obj[open + 1..close])
}
