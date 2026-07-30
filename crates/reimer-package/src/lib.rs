//! Static discovery, validation, and AST rewriting for Reimer modules.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use reimer_ast::{
    self as ast, Expression, ImportKind, Item, Pattern, Statement, TypeName, TypeNameKind,
};
use reimer_diagnostics::{Diagnostic, Span};

type ModuleName = Vec<String>;

const PACKAGE_MODULE_PREFIX: &str = "$package$";

/// A dependency edge visible from one source package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDependency {
    /// Import name used by source code.
    pub alias: String,
    /// Stable identifier of the target package in the graph.
    pub package: String,
}

/// Filesystem layout and direct dependencies for one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePackage {
    /// Stable identifier unique within the graph.
    pub id: String,
    /// Human-readable package name used in diagnostics.
    pub name: String,
    /// Root containing the package's module files.
    pub source_root: PathBuf,
    /// Entry module for this compilation.
    pub entry: PathBuf,
    /// Only dependencies listed here are importable from this package.
    pub dependencies: Vec<SourceDependency>,
}

/// Complete package graph supplied by the declarative build system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceGraph {
    /// Identifier of the package containing the program entry point.
    pub root: String,
    /// Root package and every resolved direct or transitive dependency.
    pub packages: Vec<SourcePackage>,
}

/// One loaded package rewritten into the resolver's canonical single-program form.
#[derive(Debug)]
pub struct Package {
    /// All declarations with imports removed and cross-module names canonicalized.
    pub program: ast::Program,
    sources: Vec<SourceFile>,
}

impl Package {
    /// Associates resolver diagnostics with their original source files.
    #[must_use]
    pub fn map_diagnostics(&self, diagnostics: Vec<Diagnostic>) -> Vec<FileDiagnostic> {
        diagnostics
            .into_iter()
            .map(|diagnostic| self.map_diagnostic(diagnostic))
            .collect()
    }

    fn map_diagnostic(&self, mut diagnostic: Diagnostic) -> FileDiagnostic {
        let source = self
            .sources
            .iter()
            .find(|source| source.contains(diagnostic.span))
            .or_else(|| self.sources.first());
        let Some(source) = source else {
            return FileDiagnostic {
                path: PathBuf::from("<package>"),
                source: String::new(),
                diagnostic,
            };
        };
        diagnostic.span = source.local_span(diagnostic.span);
        FileDiagnostic {
            path: source.path.clone(),
            source: source.text.clone(),
            diagnostic,
        }
    }
}

/// A compiler diagnostic paired with the source file it describes.
#[derive(Debug, Clone)]
pub struct FileDiagnostic {
    /// Original source path.
    pub path: PathBuf,
    /// Original source contents.
    pub source: String,
    /// File-local compiler diagnostic.
    pub diagnostic: Diagnostic,
}

impl FileDiagnostic {
    /// Renders this diagnostic with its original path and source line.
    #[must_use]
    pub fn render(&self) -> String {
        self.diagnostic
            .render(&self.path.display().to_string(), &self.source)
    }
}

#[derive(Debug, Clone)]
struct SourceFile {
    path: PathBuf,
    text: String,
    base: usize,
}

impl SourceFile {
    fn contains(&self, span: Span) -> bool {
        span.start >= self.base && span.start <= self.base.saturating_add(self.text.len())
    }

    fn local_span(&self, span: Span) -> Span {
        Span::new(
            span.start.saturating_sub(self.base),
            span.end.saturating_sub(self.base),
        )
    }
}

struct Module {
    name: ModuleName,
    path: PathBuf,
    is_facade: bool,
    program: ast::Program,
    dependencies: Vec<Dependency>,
}

enum ModuleLookup {
    Found(PathBuf),
    Missing,
    Ambiguous { direct: PathBuf, facade: PathBuf },
}

#[derive(Debug, Clone)]
struct Dependency {
    target: ModuleName,
    span: Span,
}

#[derive(Debug, Clone)]
struct Symbol {
    canonical: String,
}

#[derive(Debug, Clone, Default)]
struct Scope {
    symbols: HashMap<String, Symbol>,
    modules: HashMap<String, ModuleName>,
    absolute_modules: HashMap<ModuleName, ModuleName>,
}

type ModuleApis = HashMap<ModuleName, Scope>;

/// Loads an entry file and every statically imported module.
///
/// # Errors
///
/// Returns file-aware diagnostics for I/O, syntax, missing modules, duplicate
/// names, private imports, re-export failures, or import cycles.
pub fn load(entry: &Path) -> Result<Package, Vec<FileDiagnostic>> {
    Loader::single(entry).load()
}

/// Loads a resolved package graph and enforces dependency visibility per edge.
///
/// # Errors
///
/// Returns file-aware diagnostics for malformed graph metadata, I/O, syntax,
/// imports, visibility, or cycles.
pub fn load_graph(graph: &SourceGraph) -> Result<Package, Vec<FileDiagnostic>> {
    Loader::from_graph(graph)?.load()
}

struct Loader {
    root_package: String,
    packages: HashMap<String, SourcePackage>,
    modules: Vec<Module>,
    module_indices: HashMap<ModuleName, usize>,
    sources: Vec<SourceFile>,
    next_base: usize,
    diagnostics: Vec<FileDiagnostic>,
}

impl Loader {
    fn single(entry: &Path) -> Self {
        let entry = entry.to_path_buf();
        let source_root = entry
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let root_package = "root".to_owned();
        let package = SourcePackage {
            id: root_package.clone(),
            name: "root".to_owned(),
            source_root,
            entry,
            dependencies: Vec::new(),
        };
        Self {
            root_package: root_package.clone(),
            packages: HashMap::from([(root_package, package)]),
            modules: Vec::new(),
            module_indices: HashMap::new(),
            sources: Vec::new(),
            next_base: 0,
            diagnostics: Vec::new(),
        }
    }

    fn from_graph(graph: &SourceGraph) -> Result<Self, Vec<FileDiagnostic>> {
        let mut packages = HashMap::with_capacity(graph.packages.len());
        for package in &graph.packages {
            if package.id.is_empty() {
                return Err(vec![graph_diagnostic(
                    &package.entry,
                    "package graph contains an empty package identifier",
                )]);
            }
            if packages
                .insert(package.id.clone(), package.clone())
                .is_some()
            {
                return Err(vec![graph_diagnostic(
                    &package.entry,
                    format!(
                        "package graph contains duplicate identifier `{}`",
                        package.id
                    ),
                )]);
            }
        }
        let Some(root) = packages.get(&graph.root) else {
            return Err(vec![graph_diagnostic(
                Path::new("reimer.toml"),
                format!("package graph root `{}` does not exist", graph.root),
            )]);
        };
        for package in packages.values() {
            for dependency in &package.dependencies {
                if !packages.contains_key(&dependency.package) {
                    return Err(vec![graph_diagnostic(
                        &package.entry,
                        format!(
                            "dependency `{}` targets unknown package `{}`",
                            dependency.alias, dependency.package
                        ),
                    )]);
                }
            }
        }
        let root_entry = root.entry.clone();
        Ok(Self {
            root_package: graph.root.clone(),
            packages,
            modules: Vec::new(),
            module_indices: HashMap::new(),
            sources: Vec::new(),
            next_base: 0,
            diagnostics: Vec::new(),
        }
        .with_root_entry(root_entry))
    }

    fn with_root_entry(mut self, entry: PathBuf) -> Self {
        if let Some(root) = self.packages.get_mut(&self.root_package) {
            root.entry = entry;
        }
        self
    }

    fn load(mut self) -> Result<Package, Vec<FileDiagnostic>> {
        let entry = self.packages.get(&self.root_package).map_or_else(
            || PathBuf::from("src/main.reim"),
            |package| package.entry.clone(),
        );
        self.load_module(&Vec::new(), entry, None);
        if self.diagnostics.is_empty() {
            self.validate_cycles();
        }
        if !self.diagnostics.is_empty() {
            return Err(self.diagnostics);
        }

        let apis = self.build_apis();
        if !self.diagnostics.is_empty() {
            return Err(self.diagnostics);
        }
        let scopes = self.build_scopes(&apis);
        if !self.diagnostics.is_empty() {
            return Err(self.diagnostics);
        }

        let mut items = Vec::new();
        for (module, scope) in self.modules.iter().zip(&scopes) {
            let mut program = module.program.clone();
            rewrite_program(
                &mut program,
                &module.name,
                scope,
                &apis,
                &mut self.diagnostics,
            );
            items.extend(
                program
                    .items
                    .into_iter()
                    .filter(|item| !matches!(item, Item::Import(_))),
            );
        }
        if self.diagnostics.is_empty() {
            Ok(Package {
                program: ast::Program { items },
                sources: self.sources,
            })
        } else {
            Err(self.diagnostics)
        }
    }

    fn load_module(
        &mut self,
        name: &ModuleName,
        path: PathBuf,
        requested_from: Option<(&SourceFile, Span)>,
    ) {
        if self.module_indices.contains_key(name) {
            return;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                let diagnostic = Diagnostic::error(
                    "E4001",
                    format!(
                        "failed to read module `{}`: {error}",
                        self.display_module(name)
                    ),
                    requested_from.map_or(Span::empty(0), |(_, span)| span),
                );
                self.push_requested_diagnostic(requested_from, &path, diagnostic);
                return;
            }
        };
        let source = SourceFile {
            path: path.clone(),
            text,
            base: self.next_base,
        };
        self.next_base = self
            .next_base
            .saturating_add(source.text.len())
            .saturating_add(1);
        let Some(program) = self.parse_source(&source) else {
            self.sources.push(source);
            return;
        };
        let is_facade = is_facade_path(&path);
        let dependencies = self.discover_dependencies(name, is_facade, &program, &source);
        let index = self.modules.len();
        self.module_indices.insert(name.clone(), index);
        self.sources.push(source.clone());
        self.modules.push(Module {
            name: name.clone(),
            is_facade,
            path,
            program,
            dependencies: dependencies
                .iter()
                .map(|(target, span)| Dependency {
                    target: target.clone(),
                    span: *span,
                })
                .collect(),
        });
        self.load_dependencies(dependencies, &source);
    }

    fn discover_dependencies(
        &mut self,
        module: &ModuleName,
        is_facade: bool,
        program: &ast::Program,
        source: &SourceFile,
    ) -> Vec<(ModuleName, Span)> {
        let mut dependencies = Vec::new();
        let mut dependency_names = HashSet::new();
        for item in &program.items {
            let Item::Import(import) = item else {
                continue;
            };
            let import_path = match &import.kind {
                ImportKind::Module { path, .. } => path,
                ImportKind::Symbols { module, .. } => module,
            };
            let Ok(target) = self.resolve_module_name(module, is_facade, import_path) else {
                let diagnostic = Diagnostic::error(
                    "E4007",
                    "relative module path escapes the package root",
                    import_path.span,
                )
                .with_help("remove one or more leading `super::` segments");
                self.push_source_diagnostic(source, diagnostic);
                continue;
            };
            dependency_names.insert(target.clone());
            dependencies.push((target, import.span));
        }
        for qualified_path in collect_qualified_paths(program) {
            for prefix_length in (1..qualified_path.segments.len()).rev() {
                let Ok(candidate) = self.resolve_segments(
                    module,
                    is_facade,
                    &qualified_path.segments[..prefix_length],
                ) else {
                    continue;
                };
                match self.module_path(&candidate) {
                    ModuleLookup::Found(_) => {
                        if dependency_names.insert(candidate.clone()) {
                            dependencies.push((candidate, qualified_path.span));
                        }
                        break;
                    }
                    ModuleLookup::Ambiguous { direct, facade } => {
                        self.push_source_diagnostic(
                            source,
                            ambiguous_module_diagnostic(
                                &self.display_module(&candidate),
                                &direct,
                                &facade,
                                qualified_path.span,
                            ),
                        );
                        break;
                    }
                    ModuleLookup::Missing => {}
                }
            }
        }
        dependencies
    }

    fn load_dependencies(&mut self, dependencies: Vec<(ModuleName, Span)>, source: &SourceFile) {
        for (target, span) in dependencies {
            let target_path = match self.module_path(&target) {
                ModuleLookup::Found(path) => path,
                ModuleLookup::Missing => {
                    let diagnostic = Diagnostic::error(
                        "E4001",
                        format!("cannot find module `{}`", self.display_module(&target)),
                        span,
                    )
                    .with_help("add the matching `.reim` file or `package.reim` facade");
                    self.push_source_diagnostic(source, diagnostic);
                    continue;
                }
                ModuleLookup::Ambiguous { direct, facade } => {
                    self.push_source_diagnostic(
                        source,
                        ambiguous_module_diagnostic(
                            &self.display_module(&target),
                            &direct,
                            &facade,
                            span,
                        ),
                    );
                    continue;
                }
            };
            self.load_module(&target, target_path, Some((source, span)));
        }
    }

    fn push_source_diagnostic(&mut self, source: &SourceFile, diagnostic: Diagnostic) {
        self.diagnostics.push(FileDiagnostic {
            path: source.path.clone(),
            source: source.text.clone(),
            diagnostic: localize_diagnostic(diagnostic, source),
        });
    }

    fn parse_source(&mut self, source: &SourceFile) -> Option<ast::Program> {
        let mut tokens = match reimer_lexer::lex(&source.text) {
            Ok(tokens) => tokens,
            Err(diagnostics) => {
                self.diagnostics
                    .extend(diagnostics.into_iter().map(|diagnostic| FileDiagnostic {
                        path: source.path.clone(),
                        source: source.text.clone(),
                        diagnostic,
                    }));
                return None;
            }
        };
        for token in &mut tokens {
            token.span = shift_span(token.span, source.base);
        }
        match reimer_parser::parse(&tokens) {
            Ok(program) => Some(program),
            Err(diagnostics) => {
                self.diagnostics
                    .extend(diagnostics.into_iter().map(|mut diagnostic| {
                        diagnostic.span = source.local_span(diagnostic.span);
                        FileDiagnostic {
                            path: source.path.clone(),
                            source: source.text.clone(),
                            diagnostic,
                        }
                    }));
                None
            }
        }
    }

    fn module_path(&self, name: &ModuleName) -> ModuleLookup {
        let (root, entry, segments) = if name.first().is_some_and(|segment| segment == "std") {
            (standard_library_root(), None, &name[1..])
        } else {
            let Some((package, segments)) = self.package_and_segments(name) else {
                return ModuleLookup::Missing;
            };
            (
                package.source_root.clone(),
                Some(package.entry.clone()),
                segments,
            )
        };
        if segments.is_empty() {
            return entry.map_or(ModuleLookup::Missing, ModuleLookup::Found);
        }
        let mut direct = root.clone();
        for segment in segments {
            direct.push(segment);
        }
        direct.set_extension("reim");
        let mut facade = root;
        for segment in segments {
            facade.push(segment);
        }
        facade.push("package.reim");
        let direct_exists = !segments.is_empty() && direct.is_file();
        match (direct_exists, facade.is_file()) {
            (true, false) => ModuleLookup::Found(direct),
            (false, true) => ModuleLookup::Found(facade),
            (true, true) => ModuleLookup::Ambiguous { direct, facade },
            (false, false) => ModuleLookup::Missing,
        }
    }

    fn package_and_segments<'name>(
        &self,
        name: &'name ModuleName,
    ) -> Option<(&SourcePackage, &'name [String])> {
        if let Some(marker) = name
            .first()
            .and_then(|segment| segment.strip_prefix(PACKAGE_MODULE_PREFIX))
        {
            return self
                .packages
                .get(marker)
                .map(|package| (package, &name[1..]));
        }
        self.packages
            .get(&self.root_package)
            .map(|package| (package, name.as_slice()))
    }

    fn resolve_module_name(
        &self,
        current: &ModuleName,
        is_facade: bool,
        path: &ast::Path,
    ) -> Result<ModuleName, ()> {
        self.resolve_segments(current, is_facade, &path.segments)
    }

    fn resolve_segments(
        &self,
        current: &ModuleName,
        is_facade: bool,
        segments: &[ast::Identifier],
    ) -> Result<ModuleName, ()> {
        let Some((package, current_local)) = self.package_and_segments(current) else {
            return Err(());
        };
        let mut names = segments
            .iter()
            .map(|segment| segment.name.clone())
            .collect::<Vec<_>>();
        if names.first().is_some_and(|segment| segment == "self") {
            names.remove(0);
            let mut resolved = module_directory(current_local, is_facade);
            resolved.extend(names);
            return Ok(self.qualify_module(&package.id, resolved));
        }
        if names.first().is_some_and(|segment| segment == "super") {
            let mut resolved = module_directory(current_local, is_facade);
            while names.first().is_some_and(|segment| segment == "super") {
                names.remove(0);
                if resolved.pop().is_none() {
                    return Err(());
                }
            }
            resolved.extend(names);
            return Ok(self.qualify_module(&package.id, resolved));
        }
        if names.first().is_some_and(|segment| segment == "std") {
            return Ok(names);
        }
        if let Some(first) = names.first()
            && let Some(dependency) = package
                .dependencies
                .iter()
                .find(|dependency| dependency.alias == *first)
        {
            names.remove(0);
            return Ok(self.qualify_module(&dependency.package, names));
        }
        Ok(self.qualify_module(&package.id, names))
    }

    fn qualify_module(&self, package: &str, local: ModuleName) -> ModuleName {
        if package == self.root_package {
            return local;
        }
        let mut qualified = Vec::with_capacity(local.len().saturating_add(1));
        qualified.push(format!("{PACKAGE_MODULE_PREFIX}{package}"));
        qualified.extend(local);
        qualified
    }

    fn display_module(&self, module: &ModuleName) -> String {
        if module.first().is_some_and(|segment| segment == "std") {
            return module.join("::");
        }
        let Some((package, local)) = self.package_and_segments(module) else {
            return module.join("::");
        };
        if package.id == self.root_package {
            if local.is_empty() {
                "<entry>".to_owned()
            } else {
                local.join("::")
            }
        } else if local.is_empty() {
            package.name.clone()
        } else {
            format!("{}::{}", package.name, local.join("::"))
        }
    }

    fn push_requested_diagnostic(
        &mut self,
        requested_from: Option<(&SourceFile, Span)>,
        path: &Path,
        diagnostic: Diagnostic,
    ) {
        if let Some((source, _)) = requested_from {
            self.diagnostics.push(FileDiagnostic {
                path: source.path.clone(),
                source: source.text.clone(),
                diagnostic: localize_diagnostic(diagnostic, source),
            });
        } else {
            self.diagnostics.push(FileDiagnostic {
                path: path.to_path_buf(),
                source: String::new(),
                diagnostic,
            });
        }
    }

    fn validate_cycles(&mut self) {
        let mut visiting = Vec::new();
        let mut completed = HashSet::new();
        let modules = self
            .modules
            .iter()
            .map(|module| module.name.clone())
            .collect::<Vec<_>>();
        for module in modules {
            self.visit_cycle(&module, &mut visiting, &mut completed);
        }
    }

    fn visit_cycle(
        &mut self,
        module: &ModuleName,
        visiting: &mut Vec<ModuleName>,
        completed: &mut HashSet<ModuleName>,
    ) {
        if completed.contains(module) {
            return;
        }
        if let Some(start) = visiting.iter().position(|candidate| candidate == module) {
            let mut chain = visiting[start..]
                .iter()
                .map(|module| self.display_module(module))
                .collect::<Vec<_>>();
            chain.push(self.display_module(module));
            let (source, span) = self
                .module_indices
                .get(module)
                .and_then(|index| self.modules.get(*index))
                .and_then(|module| {
                    let span = module.dependencies.first()?.span;
                    let source = self
                        .sources
                        .iter()
                        .find(|source| source.path == module.path)?;
                    Some((source, span))
                })
                .map_or_else(
                    || (self.sources.first(), Span::empty(0)),
                    |(source, span)| (Some(source), span),
                );
            let diagnostic = Diagnostic::error(
                "E4002",
                format!("module import cycle: {}", chain.join(" -> ")),
                span,
            );
            if let Some(source) = source {
                self.diagnostics.push(FileDiagnostic {
                    path: source.path.clone(),
                    source: source.text.clone(),
                    diagnostic: localize_diagnostic(diagnostic, source),
                });
            }
            return;
        }
        visiting.push(module.clone());
        let dependencies = self
            .module_indices
            .get(module)
            .and_then(|index| self.modules.get(*index))
            .map(|module| module.dependencies.clone())
            .unwrap_or_default();
        for dependency in dependencies {
            self.visit_cycle(&dependency.target, visiting, completed);
        }
        visiting.pop();
        completed.insert(module.clone());
    }

    fn build_apis(&mut self) -> ModuleApis {
        let mut apis = HashMap::new();
        let mut visiting = HashSet::new();
        let modules = self
            .modules
            .iter()
            .map(|module| module.name.clone())
            .collect::<Vec<_>>();
        for module in modules {
            self.build_module_api(&module, &mut apis, &mut visiting);
        }
        apis
    }

    fn build_module_api(
        &mut self,
        name: &ModuleName,
        apis: &mut ModuleApis,
        visiting: &mut HashSet<ModuleName>,
    ) -> Scope {
        if let Some(api) = apis.get(name) {
            return api.clone();
        }
        if !visiting.insert(name.clone()) {
            return Scope::default();
        }
        let Some(index) = self.module_indices.get(name).copied() else {
            return Scope::default();
        };
        let module_name = self.modules[index].name.clone();
        let is_facade = self.modules[index].is_facade;
        let items = self.modules[index].program.items.clone();
        let mut api = Scope::default();
        self.add_public_declarations(&module_name, &items, &mut api);
        self.add_public_imports(&module_name, is_facade, &items, &mut api, apis, visiting);
        visiting.remove(name);
        apis.insert(name.clone(), api.clone());
        api
    }

    fn add_public_declarations(
        &mut self,
        module_name: &ModuleName,
        items: &[Item],
        api: &mut Scope,
    ) {
        for item in items {
            match item {
                Item::Function(function) if function.is_public => {
                    insert_symbol(
                        api,
                        function.name.name.clone(),
                        Symbol {
                            canonical: canonical_name(module_name, &function.name.name),
                        },
                        function.name.span,
                        &self.sources,
                        &mut self.diagnostics,
                    );
                }
                Item::ExternFunction(function) if function.is_public => {
                    insert_symbol(
                        api,
                        function.name.name.clone(),
                        Symbol {
                            canonical: canonical_name(module_name, &function.name.name),
                        },
                        function.name.span,
                        &self.sources,
                        &mut self.diagnostics,
                    );
                }
                Item::Struct(declaration) if declaration.is_public => {
                    insert_symbol(
                        api,
                        declaration.name.name.clone(),
                        Symbol {
                            canonical: canonical_name(module_name, &declaration.name.name),
                        },
                        declaration.name.span,
                        &self.sources,
                        &mut self.diagnostics,
                    );
                }
                Item::Enum(declaration) if declaration.is_public => {
                    insert_symbol(
                        api,
                        declaration.name.name.clone(),
                        Symbol {
                            canonical: canonical_name(module_name, &declaration.name.name),
                        },
                        declaration.name.span,
                        &self.sources,
                        &mut self.diagnostics,
                    );
                }
                Item::Trait(declaration) if declaration.is_public => {
                    insert_symbol(
                        api,
                        declaration.name.name.clone(),
                        Symbol {
                            canonical: canonical_name(module_name, &declaration.name.name),
                        },
                        declaration.name.span,
                        &self.sources,
                        &mut self.diagnostics,
                    );
                }
                Item::Constant(declaration) if declaration.is_public => {
                    insert_symbol(
                        api,
                        declaration.name.name.clone(),
                        Symbol {
                            canonical: canonical_name(module_name, &declaration.name.name),
                        },
                        declaration.name.span,
                        &self.sources,
                        &mut self.diagnostics,
                    );
                }
                _ => {}
            }
        }
    }

    fn add_public_imports(
        &mut self,
        module_name: &ModuleName,
        is_facade: bool,
        items: &[Item],
        api: &mut Scope,
        apis: &mut ModuleApis,
        visiting: &mut HashSet<ModuleName>,
    ) {
        for item in items {
            let Item::Import(import) = item else {
                continue;
            };
            if !import.is_public {
                continue;
            }
            match &import.kind {
                ImportKind::Module { path, alias } => {
                    let Ok(target) = self.resolve_module_name(module_name, is_facade, path) else {
                        continue;
                    };
                    let local = alias
                        .as_ref()
                        .map_or_else(|| last_segment(path), |alias| alias.name.clone());
                    insert_module(
                        api,
                        local,
                        target,
                        import.span,
                        &self.sources,
                        &mut self.diagnostics,
                    );
                }
                ImportKind::Symbols { module, names } => {
                    let Ok(target) = self.resolve_module_name(module_name, is_facade, module)
                    else {
                        continue;
                    };
                    let target_api = self.build_module_api(&target, apis, visiting);
                    for imported in names {
                        let local = imported
                            .alias
                            .as_ref()
                            .map_or_else(|| imported.name.name.clone(), |alias| alias.name.clone());
                        if let Some(symbol) = target_api.symbols.get(&imported.name.name).cloned() {
                            insert_symbol(
                                api,
                                local,
                                symbol,
                                imported.name.span,
                                &self.sources,
                                &mut self.diagnostics,
                            );
                        } else {
                            self.push_global_diagnostic(
                                Diagnostic::error(
                                    "E4003",
                                    format!(
                                        "`{}` is not public in module `{}`",
                                        imported.name.name,
                                        self.display_module(&target)
                                    ),
                                    imported.name.span,
                                )
                                .with_help("mark the declaration `pub` or re-export it"),
                            );
                        }
                    }
                }
            }
        }
    }

    fn build_scopes(&mut self, apis: &ModuleApis) -> Vec<Scope> {
        let mut scopes = Vec::with_capacity(self.modules.len());
        let module_data = self
            .modules
            .iter()
            .map(|module| {
                (
                    module.name.clone(),
                    module.is_facade,
                    module.program.items.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (module_name, is_facade, items) in module_data {
            let mut scope = Scope::default();
            self.add_visible_modules(&module_name, &mut scope);
            self.add_declarations_to_scope(&module_name, &items, &mut scope);
            self.add_imports_to_scope(&module_name, is_facade, &items, apis, &mut scope);
            scopes.push(scope);
        }
        scopes
    }

    fn add_visible_modules(&self, current: &ModuleName, scope: &mut Scope) {
        let Some((package, _)) = self.package_and_segments(current) else {
            return;
        };
        for module in self.module_indices.keys() {
            if module.first().is_some_and(|segment| segment == "std") {
                scope
                    .absolute_modules
                    .insert(module.clone(), module.clone());
                continue;
            }
            let Some((target_package, local)) = self.package_and_segments(module) else {
                continue;
            };
            let visible = if target_package.id == package.id {
                Some(local.to_vec())
            } else {
                package
                    .dependencies
                    .iter()
                    .find(|dependency| dependency.package == target_package.id)
                    .map(|dependency| {
                        let mut path = Vec::with_capacity(local.len().saturating_add(1));
                        path.push(dependency.alias.clone());
                        path.extend_from_slice(local);
                        path
                    })
            };
            if let Some(path) = visible {
                scope.absolute_modules.insert(path, module.clone());
            }
        }
    }

    fn add_declarations_to_scope(
        &mut self,
        module_name: &ModuleName,
        items: &[Item],
        scope: &mut Scope,
    ) {
        for item in items {
            let declaration = match item {
                Item::Function(function) => Some((function.name.name.clone(), function.name.span)),
                Item::ExternFunction(function) => {
                    Some((function.name.name.clone(), function.name.span))
                }
                Item::Struct(declaration) => {
                    Some((declaration.name.name.clone(), declaration.name.span))
                }
                Item::Enum(declaration) => {
                    Some((declaration.name.name.clone(), declaration.name.span))
                }
                Item::Trait(declaration) => {
                    Some((declaration.name.name.clone(), declaration.name.span))
                }
                Item::Constant(declaration) => {
                    Some((declaration.name.name.clone(), declaration.name.span))
                }
                Item::Import(_) | Item::Impl(_) | Item::Comptime(_) => None,
            };
            if let Some((name, span)) = declaration {
                insert_symbol(
                    scope,
                    name.clone(),
                    Symbol {
                        canonical: canonical_name(module_name, &name),
                    },
                    span,
                    &self.sources,
                    &mut self.diagnostics,
                );
            }
        }
    }

    fn add_imports_to_scope(
        &mut self,
        module_name: &ModuleName,
        is_facade: bool,
        items: &[Item],
        apis: &ModuleApis,
        scope: &mut Scope,
    ) {
        for item in items {
            let Item::Import(import) = item else {
                continue;
            };
            match &import.kind {
                ImportKind::Module { path, alias } => {
                    let Ok(target) = self.resolve_module_name(module_name, is_facade, path) else {
                        continue;
                    };
                    let local = alias
                        .as_ref()
                        .map_or_else(|| last_segment(path), |alias| alias.name.clone());
                    insert_module(
                        scope,
                        local,
                        target,
                        import.span,
                        &self.sources,
                        &mut self.diagnostics,
                    );
                }
                ImportKind::Symbols { module, names } => {
                    let Ok(target) = self.resolve_module_name(module_name, is_facade, module)
                    else {
                        continue;
                    };
                    let target_api = apis.get(&target).cloned().unwrap_or_default();
                    for imported in names {
                        let local = imported
                            .alias
                            .as_ref()
                            .map_or_else(|| imported.name.name.clone(), |alias| alias.name.clone());
                        if let Some(symbol) = target_api.symbols.get(&imported.name.name).cloned() {
                            insert_symbol(
                                scope,
                                local,
                                symbol,
                                imported.name.span,
                                &self.sources,
                                &mut self.diagnostics,
                            );
                        } else {
                            self.push_global_diagnostic(
                                Diagnostic::error(
                                    "E4003",
                                    format!(
                                        "`{}` is not public in module `{}`",
                                        imported.name.name,
                                        self.display_module(&target)
                                    ),
                                    imported.name.span,
                                )
                                .with_help("mark the declaration `pub` or re-export it"),
                            );
                        }
                    }
                }
            }
        }
    }

    fn push_global_diagnostic(&mut self, diagnostic: Diagnostic) {
        let source = self
            .sources
            .iter()
            .find(|source| source.contains(diagnostic.span))
            .or_else(|| self.sources.first());
        if let Some(source) = source {
            self.diagnostics.push(FileDiagnostic {
                path: source.path.clone(),
                source: source.text.clone(),
                diagnostic: localize_diagnostic(diagnostic, source),
            });
        }
    }
}

fn insert_symbol(
    scope: &mut Scope,
    name: String,
    symbol: Symbol,
    span: Span,
    sources: &[SourceFile],
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    if scope.symbols.contains_key(&name) || scope.modules.contains_key(&name) {
        push_duplicate(&name, span, sources, diagnostics);
    } else {
        scope.symbols.insert(name, symbol);
    }
}

fn insert_module(
    scope: &mut Scope,
    name: String,
    module: ModuleName,
    span: Span,
    sources: &[SourceFile],
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    if scope.symbols.contains_key(&name) || scope.modules.contains_key(&name) {
        push_duplicate(&name, span, sources, diagnostics);
    } else {
        scope.modules.insert(name, module);
    }
}

fn push_duplicate(
    name: &str,
    span: Span,
    sources: &[SourceFile],
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    let diagnostic = Diagnostic::error(
        "E4004",
        format!("name `{name}` is introduced more than once in this module"),
        span,
    )
    .with_help("rename the declaration or add an import alias");
    if let Some(source) = sources.iter().find(|source| source.contains(span)) {
        diagnostics.push(FileDiagnostic {
            path: source.path.clone(),
            source: source.text.clone(),
            diagnostic: localize_diagnostic(diagnostic, source),
        });
    }
}

fn ambiguous_module_diagnostic(
    module: &str,
    direct: &Path,
    facade: &Path,
    span: Span,
) -> Diagnostic {
    Diagnostic::error(
        "E4006",
        format!(
            "module `{}` is ambiguous between `{}` and `{}`",
            module,
            direct.display(),
            facade.display()
        ),
        span,
    )
    .with_help("keep either the direct module or its `package.reim` facade")
}

fn collect_qualified_paths(program: &ast::Program) -> Vec<&ast::Path> {
    let mut paths = Vec::new();
    for item in &program.items {
        match item {
            Item::Import(_) => {}
            Item::Function(function) => {
                visit_generic_parameters(&function.generic_parameters, &mut paths);
                for parameter in &function.parameters {
                    visit_type_paths(&parameter.ty, &mut paths);
                }
                if let Some(return_type) = &function.return_type {
                    visit_type_paths(return_type, &mut paths);
                }
                visit_where_predicates(&function.where_predicates, &mut paths);
                visit_block_paths(&function.body, &mut paths);
            }
            Item::ExternFunction(function) => {
                for parameter in &function.parameters {
                    visit_type_paths(&parameter.ty, &mut paths);
                }
                if let Some(return_type) = &function.return_type {
                    visit_type_paths(return_type, &mut paths);
                }
            }
            Item::Struct(declaration) => {
                visit_generic_parameters(&declaration.generic_parameters, &mut paths);
                for field in &declaration.fields {
                    visit_type_paths(&field.ty, &mut paths);
                }
                visit_where_predicates(&declaration.where_predicates, &mut paths);
            }
            Item::Enum(declaration) => {
                visit_generic_parameters(&declaration.generic_parameters, &mut paths);
                for variant in &declaration.variants {
                    match &variant.payload {
                        ast::EnumVariantPayload::Unit => {}
                        ast::EnumVariantPayload::Tuple(types) => {
                            for ty in types {
                                visit_type_paths(ty, &mut paths);
                            }
                        }
                        ast::EnumVariantPayload::Struct(fields) => {
                            for field in fields {
                                visit_type_paths(&field.ty, &mut paths);
                            }
                        }
                    }
                }
                visit_where_predicates(&declaration.where_predicates, &mut paths);
            }
            Item::Trait(declaration) => {
                visit_generic_parameters(&declaration.generic_parameters, &mut paths);
                paths.extend(declaration.supertraits.iter());
                visit_where_predicates(&declaration.where_predicates, &mut paths);
                for method in &declaration.methods {
                    visit_generic_parameters(&method.generic_parameters, &mut paths);
                    for parameter in &method.parameters {
                        visit_type_paths(&parameter.ty, &mut paths);
                    }
                    if let Some(return_type) = &method.return_type {
                        visit_type_paths(return_type, &mut paths);
                    }
                    visit_where_predicates(&method.where_predicates, &mut paths);
                }
            }
            Item::Impl(declaration) => {
                visit_generic_parameters(&declaration.generic_parameters, &mut paths);
                if let Some(trait_type) = &declaration.trait_type {
                    visit_type_paths(trait_type, &mut paths);
                }
                visit_type_paths(&declaration.target, &mut paths);
                visit_where_predicates(&declaration.where_predicates, &mut paths);
                for method in &declaration.methods {
                    visit_generic_parameters(&method.generic_parameters, &mut paths);
                    for parameter in &method.parameters {
                        visit_type_paths(&parameter.ty, &mut paths);
                    }
                    if let Some(return_type) = &method.return_type {
                        visit_type_paths(return_type, &mut paths);
                    }
                    visit_block_paths(&method.body, &mut paths);
                }
            }
            Item::Constant(declaration) => {
                visit_type_paths(&declaration.ty, &mut paths);
                visit_expression_paths(&declaration.value, &mut paths);
            }
            Item::Comptime(block) => visit_block_paths(&block.body, &mut paths),
        }
    }
    paths
}

fn visit_type_paths<'ast>(ty: &'ast TypeName, paths: &mut Vec<&'ast ast::Path>) {
    match &ty.kind {
        TypeNameKind::Function {
            parameters,
            return_type,
        } => {
            for parameter in parameters {
                visit_type_paths(parameter, paths);
            }
            visit_type_paths(return_type, paths);
        }
        TypeNameKind::Path(path) => paths.push(path),
        TypeNameKind::Generic { path, arguments } => {
            paths.push(path);
            for argument in arguments {
                match argument {
                    ast::GenericArgument::Type(ty) => visit_type_paths(ty, paths),
                    ast::GenericArgument::Const(value) => visit_expression_paths(value, paths),
                }
            }
        }
        TypeNameKind::Tuple(elements) => {
            for element in elements {
                visit_type_paths(element, paths);
            }
        }
        TypeNameKind::Array { element, length } => {
            visit_type_paths(element, paths);
            visit_expression_paths(length, paths);
        }
        TypeNameKind::Slice(element) => visit_type_paths(element, paths),
        TypeNameKind::Reference { target, .. } | TypeNameKind::RawPointer { target, .. } => {
            visit_type_paths(target, paths);
        }
        TypeNameKind::Unit => {}
    }
}

fn visit_generic_parameters<'ast>(
    parameters: &'ast [ast::GenericParameter],
    paths: &mut Vec<&'ast ast::Path>,
) {
    for parameter in parameters {
        match parameter {
            ast::GenericParameter::Type {
                bounds, default, ..
            } => {
                paths.extend(bounds);
                if let Some(default) = default {
                    visit_type_paths(default, paths);
                }
            }
            ast::GenericParameter::Const { ty, default, .. } => {
                visit_type_paths(ty, paths);
                if let Some(default) = default {
                    visit_expression_paths(default, paths);
                }
            }
        }
    }
}

fn visit_where_predicates<'ast>(
    predicates: &'ast [ast::WherePredicate],
    paths: &mut Vec<&'ast ast::Path>,
) {
    for predicate in predicates {
        visit_type_paths(&predicate.ty, paths);
        paths.extend(&predicate.bounds);
    }
}

fn visit_block_paths<'ast>(block: &'ast ast::Block, paths: &mut Vec<&'ast ast::Path>) {
    for statement in &block.statements {
        match statement {
            Statement::Let(statement) => {
                if let Some(ty) = &statement.ty {
                    visit_type_paths(ty, paths);
                }
                visit_expression_paths(&statement.initializer, paths);
            }
            Statement::Expression(statement) => {
                visit_expression_paths(&statement.expression, paths);
            }
            Statement::Defer(statement) => visit_expression_paths(&statement.action, paths),
            Statement::Return(statement) => {
                if let Some(value) = &statement.value {
                    visit_expression_paths(value, paths);
                }
            }
            Statement::While(statement) => {
                visit_expression_paths(&statement.condition, paths);
                visit_block_paths(&statement.body, paths);
            }
            Statement::For(statement) => {
                visit_pattern_paths(&statement.pattern, paths);
                visit_expression_paths(&statement.iterable, paths);
                visit_block_paths(&statement.body, paths);
            }
            Statement::Break(statement) => {
                if let Some(value) = &statement.value {
                    visit_expression_paths(value, paths);
                }
            }
            Statement::Continue(_) => {}
        }
    }
    if let Some(tail) = &block.tail {
        visit_expression_paths(tail, paths);
    }
}

fn visit_expression_paths<'ast>(expression: &'ast Expression, paths: &mut Vec<&'ast ast::Path>) {
    match expression {
        Expression::Integer(_)
        | Expression::Float(_)
        | Expression::Character(_)
        | Expression::String(_)
        | Expression::CString(_)
        | Expression::Boolean(_)
        | Expression::Unit(_) => {}
        Expression::Tuple(tuple) => {
            for element in &tuple.elements {
                visit_expression_paths(element, paths);
            }
        }
        Expression::Array(array) => {
            for element in &array.elements {
                visit_expression_paths(element, paths);
            }
        }
        Expression::Struct(structure) => {
            paths.push(&structure.path);
            for field in &structure.fields {
                visit_expression_paths(&field.value, paths);
            }
        }
        Expression::Path(path) => paths.push(path),
        Expression::Unary(unary) => visit_expression_paths(&unary.operand, paths),
        Expression::Binary(binary) => {
            visit_expression_paths(&binary.left, paths);
            visit_expression_paths(&binary.right, paths);
        }
        Expression::Call(call) => {
            visit_expression_paths(&call.callee, paths);
            for argument in &call.generic_arguments {
                match argument {
                    ast::GenericArgument::Type(ty) => visit_type_paths(ty, paths),
                    ast::GenericArgument::Const(value) => visit_expression_paths(value, paths),
                }
            }
            for argument in &call.arguments {
                visit_expression_paths(argument, paths);
            }
        }
        Expression::If(conditional) => {
            visit_expression_paths(&conditional.condition, paths);
            visit_block_paths(&conditional.then_branch, paths);
            if let Some(alternative) = &conditional.else_branch {
                visit_expression_paths(alternative, paths);
            }
        }
        Expression::Match(matched) => {
            visit_expression_paths(&matched.scrutinee, paths);
            for arm in &matched.arms {
                visit_pattern_paths(&arm.pattern, paths);
                if let Some(guard) = &arm.guard {
                    visit_expression_paths(guard, paths);
                }
                visit_expression_paths(&arm.body, paths);
            }
        }
        Expression::Loop(loop_expression) => visit_block_paths(&loop_expression.body, paths),
        Expression::Unsafe(block) | Expression::Block(block) => visit_block_paths(block, paths),
        Expression::Assignment(assignment) => {
            visit_expression_paths(&assignment.target, paths);
            visit_expression_paths(&assignment.value, paths);
        }
        Expression::Cast(cast) => {
            visit_expression_paths(&cast.value, paths);
            visit_type_paths(&cast.target, paths);
        }
        Expression::Field(field) => visit_expression_paths(&field.base, paths),
        Expression::Index(index) => {
            visit_expression_paths(&index.base, paths);
            for value in &index.indices {
                visit_expression_paths(value, paths);
            }
        }
        Expression::Try { value, .. } => visit_expression_paths(value, paths),
    }
}

fn visit_pattern_paths<'ast>(pattern: &'ast Pattern, paths: &mut Vec<&'ast ast::Path>) {
    match pattern {
        Pattern::Wildcard(_)
        | Pattern::Identifier { .. }
        | Pattern::Integer { .. }
        | Pattern::Float { .. }
        | Pattern::Character(_)
        | Pattern::Boolean(_) => {}
        Pattern::Tuple { elements, .. } => {
            for element in elements {
                visit_pattern_paths(element, paths);
            }
        }
        Pattern::Path(path) => paths.push(path),
        Pattern::EnumTuple { path, fields, .. } => {
            paths.push(path);
            for field in fields {
                visit_pattern_paths(field, paths);
            }
        }
        Pattern::EnumStruct { path, fields, .. } => {
            paths.push(path);
            for field in fields {
                visit_pattern_paths(&field.pattern, paths);
            }
        }
    }
}

fn rewrite_program(
    program: &mut ast::Program,
    module: &ModuleName,
    scope: &Scope,
    apis: &ModuleApis,
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    let mut context = ItemRewriteContext {
        module,
        scope,
        apis,
        diagnostics,
    };
    for item in &mut program.items {
        context.rewrite_item(item);
    }
}

struct ItemRewriteContext<'context> {
    module: &'context ModuleName,
    scope: &'context Scope,
    apis: &'context ModuleApis,
    diagnostics: &'context mut Vec<FileDiagnostic>,
}

impl ItemRewriteContext<'_> {
    fn rewrite_item(&mut self, item: &mut Item) {
        match item {
            Item::Import(_) => {}
            Item::Function(function) => self.rewrite_function(function),
            Item::ExternFunction(function) => self.rewrite_extern_function(function),
            Item::Struct(declaration) => self.rewrite_struct(declaration),
            Item::Enum(declaration) => self.rewrite_enum(declaration),
            Item::Trait(declaration) => self.rewrite_trait(declaration),
            Item::Impl(declaration) => self.rewrite_impl(declaration),
            Item::Constant(declaration) => {
                rewrite_identifier(&mut declaration.name, self.module);
                rewrite_type(&mut declaration.ty, self.scope, self.apis, self.diagnostics);
                rewrite_expression(
                    &mut declaration.value,
                    self.scope,
                    self.apis,
                    self.diagnostics,
                );
            }
            Item::Comptime(block) => {
                rewrite_block(&mut block.body, self.scope, self.apis, self.diagnostics);
            }
        }
    }

    fn rewrite_function(&mut self, function: &mut ast::Function) {
        rewrite_identifier(&mut function.name, self.module);
        self.rewrite_generics(&mut function.generic_parameters);
        self.rewrite_parameters(&mut function.parameters);
        self.rewrite_optional_type(&mut function.return_type);
        self.rewrite_predicates(&mut function.where_predicates);
        rewrite_block(&mut function.body, self.scope, self.apis, self.diagnostics);
    }

    fn rewrite_extern_function(&mut self, function: &mut ast::ExternFunction) {
        rewrite_identifier(&mut function.name, self.module);
        self.rewrite_parameters(&mut function.parameters);
        self.rewrite_optional_type(&mut function.return_type);
    }

    fn rewrite_struct(&mut self, declaration: &mut ast::StructDeclaration) {
        rewrite_identifier(&mut declaration.name, self.module);
        self.rewrite_generics(&mut declaration.generic_parameters);
        for field in &mut declaration.fields {
            rewrite_type(&mut field.ty, self.scope, self.apis, self.diagnostics);
        }
        self.rewrite_predicates(&mut declaration.where_predicates);
    }

    fn rewrite_enum(&mut self, declaration: &mut ast::EnumDeclaration) {
        rewrite_identifier(&mut declaration.name, self.module);
        self.rewrite_generics(&mut declaration.generic_parameters);
        for variant in &mut declaration.variants {
            match &mut variant.payload {
                ast::EnumVariantPayload::Unit => {}
                ast::EnumVariantPayload::Tuple(types) => {
                    for ty in types {
                        rewrite_type(ty, self.scope, self.apis, self.diagnostics);
                    }
                }
                ast::EnumVariantPayload::Struct(fields) => {
                    for field in fields {
                        rewrite_type(&mut field.ty, self.scope, self.apis, self.diagnostics);
                    }
                }
            }
        }
        self.rewrite_predicates(&mut declaration.where_predicates);
    }

    fn rewrite_trait(&mut self, declaration: &mut ast::TraitDeclaration) {
        rewrite_identifier(&mut declaration.name, self.module);
        self.rewrite_generics(&mut declaration.generic_parameters);
        for bound in &mut declaration.supertraits {
            rewrite_path(bound, self.scope, self.apis, self.diagnostics);
        }
        self.rewrite_predicates(&mut declaration.where_predicates);
        for method in &mut declaration.methods {
            self.rewrite_generics(&mut method.generic_parameters);
            self.rewrite_parameters(&mut method.parameters);
            self.rewrite_optional_type(&mut method.return_type);
            self.rewrite_predicates(&mut method.where_predicates);
        }
    }

    fn rewrite_impl(&mut self, declaration: &mut ast::ImplDeclaration) {
        self.rewrite_generics(&mut declaration.generic_parameters);
        self.rewrite_optional_type(&mut declaration.trait_type);
        rewrite_type(
            &mut declaration.target,
            self.scope,
            self.apis,
            self.diagnostics,
        );
        self.rewrite_predicates(&mut declaration.where_predicates);
        for method in &mut declaration.methods {
            self.rewrite_generics(&mut method.generic_parameters);
            self.rewrite_parameters(&mut method.parameters);
            self.rewrite_optional_type(&mut method.return_type);
            self.rewrite_predicates(&mut method.where_predicates);
            rewrite_block(&mut method.body, self.scope, self.apis, self.diagnostics);
        }
    }

    fn rewrite_generics(&mut self, parameters: &mut [ast::GenericParameter]) {
        rewrite_generic_parameters(parameters, self.scope, self.apis, self.diagnostics);
    }

    fn rewrite_parameters(&mut self, parameters: &mut [ast::Parameter]) {
        for parameter in parameters {
            rewrite_type(&mut parameter.ty, self.scope, self.apis, self.diagnostics);
        }
    }

    fn rewrite_optional_type(&mut self, ty: &mut Option<TypeName>) {
        if let Some(ty) = ty {
            rewrite_type(ty, self.scope, self.apis, self.diagnostics);
        }
    }

    fn rewrite_predicates(&mut self, predicates: &mut [ast::WherePredicate]) {
        rewrite_where_predicates(predicates, self.scope, self.apis, self.diagnostics);
    }
}

fn rewrite_identifier(identifier: &mut ast::Identifier, module: &ModuleName) {
    identifier.name = canonical_name(module, &identifier.name);
}

fn rewrite_type(
    ty: &mut TypeName,
    scope: &Scope,
    apis: &ModuleApis,
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    match &mut ty.kind {
        TypeNameKind::Function {
            parameters,
            return_type,
        } => {
            for parameter in parameters {
                rewrite_type(parameter, scope, apis, diagnostics);
            }
            rewrite_type(return_type, scope, apis, diagnostics);
        }
        TypeNameKind::Path(path) => rewrite_path(path, scope, apis, diagnostics),
        TypeNameKind::Generic { path, arguments } => {
            rewrite_path(path, scope, apis, diagnostics);
            for argument in arguments {
                match argument {
                    ast::GenericArgument::Type(ty) => {
                        rewrite_type(ty, scope, apis, diagnostics);
                    }
                    ast::GenericArgument::Const(value) => {
                        rewrite_expression(value, scope, apis, diagnostics);
                    }
                }
            }
        }
        TypeNameKind::Tuple(elements) => {
            for element in elements {
                rewrite_type(element, scope, apis, diagnostics);
            }
        }
        TypeNameKind::Array { element, length } => {
            rewrite_type(element, scope, apis, diagnostics);
            rewrite_expression(length, scope, apis, diagnostics);
        }
        TypeNameKind::Slice(element) => rewrite_type(element, scope, apis, diagnostics),
        TypeNameKind::Reference { target, .. } | TypeNameKind::RawPointer { target, .. } => {
            rewrite_type(target, scope, apis, diagnostics);
        }
        TypeNameKind::Unit => {}
    }
}

fn rewrite_generic_parameters(
    parameters: &mut [ast::GenericParameter],
    scope: &Scope,
    apis: &ModuleApis,
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    for parameter in parameters {
        match parameter {
            ast::GenericParameter::Type {
                bounds, default, ..
            } => {
                for bound in bounds {
                    rewrite_path(bound, scope, apis, diagnostics);
                }
                if let Some(default) = default {
                    rewrite_type(default, scope, apis, diagnostics);
                }
            }
            ast::GenericParameter::Const { ty, default, .. } => {
                rewrite_type(ty, scope, apis, diagnostics);
                if let Some(default) = default {
                    rewrite_expression(default, scope, apis, diagnostics);
                }
            }
        }
    }
}

fn rewrite_where_predicates(
    predicates: &mut [ast::WherePredicate],
    scope: &Scope,
    apis: &ModuleApis,
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    for predicate in predicates {
        rewrite_type(&mut predicate.ty, scope, apis, diagnostics);
        for bound in &mut predicate.bounds {
            rewrite_path(bound, scope, apis, diagnostics);
        }
    }
}

fn rewrite_block(
    block: &mut ast::Block,
    scope: &Scope,
    apis: &ModuleApis,
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    for statement in &mut block.statements {
        match statement {
            Statement::Let(statement) => {
                if let Some(ty) = &mut statement.ty {
                    rewrite_type(ty, scope, apis, diagnostics);
                }
                rewrite_expression(&mut statement.initializer, scope, apis, diagnostics);
            }
            Statement::Expression(statement) => {
                rewrite_expression(&mut statement.expression, scope, apis, diagnostics);
            }
            Statement::Defer(statement) => {
                rewrite_expression(&mut statement.action, scope, apis, diagnostics);
            }
            Statement::Return(statement) => {
                if let Some(value) = &mut statement.value {
                    rewrite_expression(value, scope, apis, diagnostics);
                }
            }
            Statement::While(statement) => {
                rewrite_expression(&mut statement.condition, scope, apis, diagnostics);
                rewrite_block(&mut statement.body, scope, apis, diagnostics);
            }
            Statement::For(statement) => {
                rewrite_pattern(&mut statement.pattern, scope, apis, diagnostics);
                rewrite_expression(&mut statement.iterable, scope, apis, diagnostics);
                rewrite_block(&mut statement.body, scope, apis, diagnostics);
            }
            Statement::Break(statement) => {
                if let Some(value) = &mut statement.value {
                    rewrite_expression(value, scope, apis, diagnostics);
                }
            }
            Statement::Continue(_) => {}
        }
    }
    if let Some(tail) = &mut block.tail {
        rewrite_expression(tail, scope, apis, diagnostics);
    }
}

fn rewrite_expression(
    expression: &mut Expression,
    scope: &Scope,
    apis: &ModuleApis,
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    match expression {
        Expression::Integer(_)
        | Expression::Float(_)
        | Expression::Character(_)
        | Expression::String(_)
        | Expression::CString(_)
        | Expression::Boolean(_)
        | Expression::Unit(_) => {}
        Expression::Tuple(tuple) => {
            for element in &mut tuple.elements {
                rewrite_expression(element, scope, apis, diagnostics);
            }
        }
        Expression::Array(array) => {
            for element in &mut array.elements {
                rewrite_expression(element, scope, apis, diagnostics);
            }
        }
        Expression::Struct(structure) => {
            rewrite_path(&mut structure.path, scope, apis, diagnostics);
            for field in &mut structure.fields {
                rewrite_expression(&mut field.value, scope, apis, diagnostics);
            }
        }
        Expression::Path(path) => rewrite_path(path, scope, apis, diagnostics),
        Expression::Unary(unary) => {
            rewrite_expression(&mut unary.operand, scope, apis, diagnostics);
        }
        Expression::Binary(binary) => {
            rewrite_expression(&mut binary.left, scope, apis, diagnostics);
            rewrite_expression(&mut binary.right, scope, apis, diagnostics);
        }
        Expression::Call(call) => {
            rewrite_expression(&mut call.callee, scope, apis, diagnostics);
            for argument in &mut call.generic_arguments {
                match argument {
                    ast::GenericArgument::Type(ty) => {
                        rewrite_type(ty, scope, apis, diagnostics);
                    }
                    ast::GenericArgument::Const(value) => {
                        rewrite_expression(value, scope, apis, diagnostics);
                    }
                }
            }
            for argument in &mut call.arguments {
                rewrite_expression(argument, scope, apis, diagnostics);
            }
        }
        Expression::If(conditional) => {
            rewrite_expression(&mut conditional.condition, scope, apis, diagnostics);
            rewrite_block(&mut conditional.then_branch, scope, apis, diagnostics);
            if let Some(alternative) = &mut conditional.else_branch {
                rewrite_expression(alternative, scope, apis, diagnostics);
            }
        }
        Expression::Match(matched) => {
            rewrite_expression(&mut matched.scrutinee, scope, apis, diagnostics);
            for arm in &mut matched.arms {
                rewrite_pattern(&mut arm.pattern, scope, apis, diagnostics);
                if let Some(guard) = &mut arm.guard {
                    rewrite_expression(guard, scope, apis, diagnostics);
                }
                rewrite_expression(&mut arm.body, scope, apis, diagnostics);
            }
        }
        Expression::Loop(loop_expression) => {
            rewrite_block(&mut loop_expression.body, scope, apis, diagnostics);
        }
        Expression::Unsafe(block) | Expression::Block(block) => {
            rewrite_block(block, scope, apis, diagnostics);
        }
        Expression::Assignment(assignment) => {
            rewrite_expression(&mut assignment.target, scope, apis, diagnostics);
            rewrite_expression(&mut assignment.value, scope, apis, diagnostics);
        }
        Expression::Cast(cast) => {
            rewrite_expression(&mut cast.value, scope, apis, diagnostics);
            rewrite_type(&mut cast.target, scope, apis, diagnostics);
        }
        Expression::Field(field) => {
            rewrite_expression(&mut field.base, scope, apis, diagnostics);
        }
        Expression::Index(index) => {
            rewrite_expression(&mut index.base, scope, apis, diagnostics);
            for value in &mut index.indices {
                rewrite_expression(value, scope, apis, diagnostics);
            }
        }
        Expression::Try { value, .. } => {
            rewrite_expression(value, scope, apis, diagnostics);
        }
    }
}

fn rewrite_pattern(
    pattern: &mut Pattern,
    scope: &Scope,
    apis: &ModuleApis,
    diagnostics: &mut Vec<FileDiagnostic>,
) {
    match pattern {
        Pattern::Wildcard(_)
        | Pattern::Identifier { .. }
        | Pattern::Integer { .. }
        | Pattern::Float { .. }
        | Pattern::Character(_)
        | Pattern::Boolean(_) => {}
        Pattern::Tuple { elements, .. } => {
            for element in elements {
                rewrite_pattern(element, scope, apis, diagnostics);
            }
        }
        Pattern::Path(path) => rewrite_path(path, scope, apis, diagnostics),
        Pattern::EnumTuple { path, fields, .. } => {
            rewrite_path(path, scope, apis, diagnostics);
            for field in fields {
                rewrite_pattern(field, scope, apis, diagnostics);
            }
        }
        Pattern::EnumStruct { path, fields, .. } => {
            rewrite_path(path, scope, apis, diagnostics);
            for field in fields {
                rewrite_pattern(&mut field.pattern, scope, apis, diagnostics);
            }
        }
    }
}

fn rewrite_path(
    path: &mut ast::Path,
    scope: &Scope,
    apis: &ModuleApis,
    _diagnostics: &mut Vec<FileDiagnostic>,
) {
    let Some(first) = path.segments.first() else {
        return;
    };
    if let Some(symbol) = scope.symbols.get(&first.name) {
        path.segments[0].name = symbol.canonical.clone();
        return;
    }
    let Some(mut module) = scope.modules.get(&first.name).cloned() else {
        rewrite_absolute_path(path, scope, apis);
        return;
    };
    let mut index = 1;
    while index < path.segments.len() {
        let Some(api) = apis.get(&module) else {
            return;
        };
        let segment = &path.segments[index].name;
        if let Some(next_module) = api.modules.get(segment) {
            module = next_module.clone();
            index += 1;
            continue;
        }
        if let Some(symbol) = api.symbols.get(segment) {
            let mut rewritten = Vec::with_capacity(path.segments.len() - index);
            rewritten.push(ast::Identifier {
                name: symbol.canonical.clone(),
                span: path.segments[index].span,
            });
            rewritten.extend(path.segments[index + 1..].iter().cloned());
            path.segments = rewritten;
        }
        return;
    }
}

fn rewrite_absolute_path(path: &mut ast::Path, scope: &Scope, apis: &ModuleApis) {
    for prefix_length in (1..path.segments.len()).rev() {
        let visible = path.segments[..prefix_length]
            .iter()
            .map(|segment| segment.name.clone())
            .collect::<ModuleName>();
        let Some(module) = scope.absolute_modules.get(&visible) else {
            continue;
        };
        let Some(api) = apis.get(module) else {
            continue;
        };
        let Some(symbol_segment) = path.segments.get(prefix_length) else {
            continue;
        };
        let Some(symbol) = api.symbols.get(&symbol_segment.name) else {
            continue;
        };
        let mut rewritten = Vec::with_capacity(path.segments.len() - prefix_length);
        rewritten.push(ast::Identifier {
            name: symbol.canonical.clone(),
            span: symbol_segment.span,
        });
        rewritten.extend(path.segments[prefix_length + 1..].iter().cloned());
        path.segments = rewritten;
        return;
    }
}

fn module_directory(module: &[String], is_facade: bool) -> ModuleName {
    let mut directory = module.to_vec();
    if !is_facade {
        directory.pop();
    }
    directory
}

fn standard_library_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("std")
}

fn is_facade_path(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("package.reim")
}

fn canonical_name(module: &ModuleName, local: &str) -> String {
    if module.is_empty() {
        return local.to_owned();
    }
    let encoded = module
        .iter()
        .map(|segment| format!("{}_{}", segment.len(), segment))
        .collect::<Vec<_>>()
        .join("_");
    format!("__module_{encoded}${local}")
}

fn last_segment(path: &ast::Path) -> String {
    path.segments
        .last()
        .map_or_else(String::new, |segment| segment.name.clone())
}

fn shift_span(span: Span, base: usize) -> Span {
    Span::new(
        span.start.saturating_add(base),
        span.end.saturating_add(base),
    )
}

fn localize_diagnostic(mut diagnostic: Diagnostic, source: &SourceFile) -> Diagnostic {
    diagnostic.span = source.local_span(diagnostic.span);
    diagnostic
}

fn graph_diagnostic(path: &Path, message: impl Into<String>) -> FileDiagnostic {
    FileDiagnostic {
        path: path.to_path_buf(),
        source: String::new(),
        diagnostic: Diagnostic::error("E4010", message, Span::empty(0)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{SourceDependency, SourceGraph, SourcePackage, load, load_graph};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let unique = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("reimer-package-{}-{unique}", std::process::id()));
            fs::create_dir_all(&root).expect("fixture directory should be created");
            Self { root }
        }

        fn write(&self, relative: &str, source: &str) {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("fixture parent should be created");
            }
            fs::write(path, source).expect("fixture source should be written");
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn load_graph_should_resolve_direct_and_transitive_package_dependencies() {
        let fixture = Fixture::new();
        fixture.write(
            "app/src/main.reim",
            "from physics import combined; fn main() -> i32 { combined() }",
        );
        fixture.write(
            "physics/src/package.reim",
            "from vectors import answer; pub fn combined() -> i32 { answer() }",
        );
        fixture.write("vectors/src/package.reim", "pub fn answer() -> i32 { 42 }");
        let graph = SourceGraph {
            root: "app".to_owned(),
            packages: vec![
                SourcePackage {
                    id: "app".to_owned(),
                    name: "app".to_owned(),
                    source_root: fixture.path("app/src"),
                    entry: fixture.path("app/src/main.reim"),
                    dependencies: vec![SourceDependency {
                        alias: "physics".to_owned(),
                        package: "physics".to_owned(),
                    }],
                },
                SourcePackage {
                    id: "physics".to_owned(),
                    name: "physics".to_owned(),
                    source_root: fixture.path("physics/src"),
                    entry: fixture.path("physics/src/package.reim"),
                    dependencies: vec![SourceDependency {
                        alias: "vectors".to_owned(),
                        package: "vectors".to_owned(),
                    }],
                },
                SourcePackage {
                    id: "vectors".to_owned(),
                    name: "vectors".to_owned(),
                    source_root: fixture.path("vectors/src"),
                    entry: fixture.path("vectors/src/package.reim"),
                    dependencies: Vec::new(),
                },
            ],
        };

        let package = load_graph(&graph).expect("package graph should load");
        let program =
            reimer_resolver::resolve(&package.program).expect("merged graph should resolve");

        assert_eq!(program.functions.len(), 3);
    }

    #[test]
    fn load_graph_should_reject_imports_of_transitive_dependencies() {
        let fixture = Fixture::new();
        fixture.write(
            "app/src/main.reim",
            "from vectors import answer; fn main() -> i32 { answer() }",
        );
        fixture.write(
            "physics/src/package.reim",
            "from vectors import answer; pub fn combined() -> i32 { answer() }",
        );
        fixture.write("vectors/src/package.reim", "pub fn answer() -> i32 { 42 }");
        let graph = SourceGraph {
            root: "app".to_owned(),
            packages: vec![
                SourcePackage {
                    id: "app".to_owned(),
                    name: "app".to_owned(),
                    source_root: fixture.path("app/src"),
                    entry: fixture.path("app/src/main.reim"),
                    dependencies: vec![SourceDependency {
                        alias: "physics".to_owned(),
                        package: "physics".to_owned(),
                    }],
                },
                SourcePackage {
                    id: "physics".to_owned(),
                    name: "physics".to_owned(),
                    source_root: fixture.path("physics/src"),
                    entry: fixture.path("physics/src/package.reim"),
                    dependencies: vec![SourceDependency {
                        alias: "vectors".to_owned(),
                        package: "vectors".to_owned(),
                    }],
                },
                SourcePackage {
                    id: "vectors".to_owned(),
                    name: "vectors".to_owned(),
                    source_root: fixture.path("vectors/src"),
                    entry: fixture.path("vectors/src/package.reim"),
                    dependencies: Vec::new(),
                },
            ],
        };

        let diagnostics = load_graph(&graph).expect_err("transitive import should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.diagnostic.code == "E4001")
        );
    }

    #[test]
    fn load_should_resolve_multiple_files_facades_aliases_and_reexports() {
        let fixture = Fixture::new();
        fixture.write(
            "main.reim",
            "import game::math as math;
             from game import Pair;
             fn main() -> i32 {
                 let pair = Pair { left: 20, right: 22 };
                 math::sum(pair)
             }",
        );
        fixture.write(
            "game/package.reim",
            "pub from self::types import Pair;
             pub import self::math as math;",
        );
        fixture.write(
            "game/types.reim",
            "pub struct Pair { pub left: i32, pub right: i32 }",
        );
        fixture.write(
            "game/math.reim",
            "from self::types import Pair;
             pub fn sum(pair: Pair) -> i32 { pair.left + pair.right }",
        );

        let package = load(&fixture.path("main.reim")).expect("package should load");
        let program =
            reimer_resolver::resolve(&package.program).expect("merged package should resolve");

        assert_eq!(program.functions.len(), 2);
    }

    #[test]
    fn load_should_reject_private_selective_imports() {
        let fixture = Fixture::new();
        fixture.write(
            "main.reim",
            "from secrets import hidden; fn main() -> i32 { hidden() }",
        );
        fixture.write("secrets.reim", "fn hidden() -> i32 { 42 }");

        let diagnostics = load(&fixture.path("main.reim")).expect_err("package should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.diagnostic.code == "E4003")
        );
    }

    #[test]
    fn load_should_reject_ambiguous_direct_and_facade_modules() {
        let fixture = Fixture::new();
        fixture.write("main.reim", "import game; fn main() -> i32 { 42 }");
        fixture.write("game.reim", "pub fn direct() -> i32 { 1 }");
        fixture.write("game/package.reim", "pub fn facade() -> i32 { 2 }");

        let diagnostics = load(&fixture.path("main.reim")).expect_err("package should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.diagnostic.code == "E4006")
        );
    }

    #[test]
    fn load_should_resolve_fully_qualified_paths_without_an_import() {
        let fixture = Fixture::new();
        fixture.write("main.reim", "fn main() -> i32 { game::math::answer() }");
        fixture.write("game/math.reim", "pub fn answer() -> i32 { 42 }");

        let package = load(&fixture.path("main.reim")).expect("package should load");
        let program =
            reimer_resolver::resolve(&package.program).expect("merged package should resolve");

        assert_eq!(program.functions.len(), 2);
    }

    #[test]
    fn load_should_reject_relative_paths_above_the_package_root() {
        let fixture = Fixture::new();
        fixture.write(
            "main.reim",
            "import super::outside; fn main() -> i32 { 42 }",
        );

        let diagnostics = load(&fixture.path("main.reim")).expect_err("package should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.diagnostic.code == "E4007")
        );
    }

    #[test]
    fn load_should_report_the_complete_import_cycle() {
        let fixture = Fixture::new();
        fixture.write("main.reim", "import a; fn main() -> i32 { 42 }");
        fixture.write("a.reim", "import b; pub fn a() -> i32 { 1 }");
        fixture.write("b.reim", "import a; pub fn b() -> i32 { 2 }");

        let diagnostics = load(&fixture.path("main.reim")).expect_err("package should fail");
        let cycle = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.diagnostic.code == "E4002")
            .expect("cycle diagnostic should exist");

        assert!(cycle.diagnostic.message.contains("a -> b -> a"));
    }

    #[test]
    fn resolver_should_reject_private_fields_across_modules() {
        let fixture = Fixture::new();
        fixture.write(
            "main.reim",
            "from secrets import Secret, make;
             fn main() -> i32 {
                 let secret = make();
                 secret.value
             }",
        );
        fixture.write(
            "secrets.reim",
            "pub struct Secret { value: i32 }
             pub fn make() -> Secret { Secret { value: 42 } }",
        );

        let package = load(&fixture.path("main.reim")).expect("package should load");
        let diagnostics =
            reimer_resolver::resolve(&package.program).expect_err("resolution should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E4005")
        );
    }

    #[test]
    fn file_diagnostic_should_render_the_importing_source() {
        let fixture = Fixture::new();
        fixture.write(
            "main.reim",
            "from missing import value; fn main() -> i32 { 42 }",
        );

        let diagnostics = load(&fixture.path("main.reim")).expect_err("package should fail");
        let rendered = diagnostics[0].render();

        assert!(rendered.contains("main.reim"));
        assert!(rendered.contains("from missing import value"));
    }

    #[test]
    fn fixture_paths_should_remain_inside_the_temporary_root() {
        let fixture = Fixture::new();
        let path = fixture.path("main.reim");

        assert!(path.starts_with(&fixture.root));
    }
}
