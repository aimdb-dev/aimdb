//! Source-level guard, not a name-resolution or macro-expansion pass.
//! Parse Rust syntax and follow modules, excluding code whose explicit `cfg`
//! cannot hold when `test = false`. Other cfg predicates remain unknown, so
//! every production feature/target combination is checked conservatively.
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use std::{
    env, fs,
    path::{Path, PathBuf},
};
use syn::{
    parse::Parser,
    punctuated::Punctuated,
    visit::{self, Visit},
    Attribute, Meta, Token,
};

// (can be false, can be true) when compiling a production library.
fn possibilities(meta: &Meta) -> (bool, bool) {
    match meta {
        Meta::Path(path) if path.is_ident("test") => (true, false),
        Meta::List(list) => {
            let Ok(args) =
                Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone())
            else {
                return (true, true);
            };
            let values: Vec<_> = args.iter().map(possibilities).collect();
            if list.path.is_ident("all") {
                (values.iter().any(|v| v.0), values.iter().all(|v| v.1))
            } else if list.path.is_ident("any") {
                (values.iter().all(|v| v.0), values.iter().any(|v| v.1))
            } else if list.path.is_ident("not") && values.len() == 1 {
                (values[0].1, values[0].0)
            } else {
                (true, true)
            }
        }
        _ => (true, true),
    }
}

fn excluded(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<Meta>()
                .is_ok_and(|meta| !possibilities(&meta).1)
    })
}

// Outer attributes of a node syn keeps only as raw tokens (`Verbatim`).
// Re-tokenising is reserved for these, since doing it for every visited node
// re-serialises each subtree once per ancestor.
fn verbatim_test_only(tokens: &TokenStream) -> bool {
    let parser = |input: syn::parse::ParseStream<'_>| {
        let attrs = input.call(Attribute::parse_outer)?;
        let _: TokenStream = input.parse()?;
        Ok(attrs)
    };
    parser
        .parse2(tokens.clone())
        .is_ok_and(|attrs| excluded(&attrs))
}

fn item_test_only(item: &syn::Item) -> bool {
    use syn::Item;
    let attrs = match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        Item::Verbatim(tokens) => return verbatim_test_only(tokens),
        _ => return false,
    };
    excluded(attrs)
}

fn impl_item_test_only(item: &syn::ImplItem) -> bool {
    use syn::ImplItem;
    let attrs = match item {
        ImplItem::Const(i) => &i.attrs,
        ImplItem::Fn(i) => &i.attrs,
        ImplItem::Type(i) => &i.attrs,
        ImplItem::Macro(i) => &i.attrs,
        ImplItem::Verbatim(tokens) => return verbatim_test_only(tokens),
        _ => return false,
    };
    excluded(attrs)
}

fn trait_item_test_only(item: &syn::TraitItem) -> bool {
    use syn::TraitItem;
    let attrs = match item {
        TraitItem::Const(i) => &i.attrs,
        TraitItem::Fn(i) => &i.attrs,
        TraitItem::Type(i) => &i.attrs,
        TraitItem::Macro(i) => &i.attrs,
        TraitItem::Verbatim(tokens) => return verbatim_test_only(tokens),
        _ => return false,
    };
    excluded(attrs)
}

fn expr_test_only(expr: &syn::Expr) -> bool {
    use syn::Expr;
    let attrs = match expr {
        Expr::Array(e) => &e.attrs,
        Expr::Assign(e) => &e.attrs,
        Expr::Async(e) => &e.attrs,
        Expr::Await(e) => &e.attrs,
        Expr::Binary(e) => &e.attrs,
        Expr::Block(e) => &e.attrs,
        Expr::Break(e) => &e.attrs,
        Expr::Call(e) => &e.attrs,
        Expr::Cast(e) => &e.attrs,
        Expr::Closure(e) => &e.attrs,
        Expr::Const(e) => &e.attrs,
        Expr::Continue(e) => &e.attrs,
        Expr::Field(e) => &e.attrs,
        Expr::ForLoop(e) => &e.attrs,
        Expr::Group(e) => &e.attrs,
        Expr::If(e) => &e.attrs,
        Expr::Index(e) => &e.attrs,
        Expr::Infer(e) => &e.attrs,
        Expr::Let(e) => &e.attrs,
        Expr::Lit(e) => &e.attrs,
        Expr::Loop(e) => &e.attrs,
        Expr::Macro(e) => &e.attrs,
        Expr::Match(e) => &e.attrs,
        Expr::MethodCall(e) => &e.attrs,
        Expr::Paren(e) => &e.attrs,
        Expr::Path(e) => &e.attrs,
        Expr::Range(e) => &e.attrs,
        Expr::RawAddr(e) => &e.attrs,
        Expr::Reference(e) => &e.attrs,
        Expr::Repeat(e) => &e.attrs,
        Expr::Return(e) => &e.attrs,
        Expr::Struct(e) => &e.attrs,
        Expr::Try(e) => &e.attrs,
        Expr::TryBlock(e) => &e.attrs,
        Expr::Tuple(e) => &e.attrs,
        Expr::Unary(e) => &e.attrs,
        Expr::Unsafe(e) => &e.attrs,
        Expr::While(e) => &e.attrs,
        Expr::Yield(e) => &e.attrs,
        Expr::Verbatim(tokens) => return verbatim_test_only(tokens),
        _ => return false,
    };
    excluded(attrs)
}

fn forbidden(name: &str) -> bool {
    matches!(
        name,
        "tracing_subscriber"
            | "set_global_default"
            | "set_boxed_logger"
            | "set_logger"
            | "set_max_level"
            | "set_hook"
            | "take_hook"
            | "pthread_atfork"
            | "sigaction"
            | "signal_hook"
            | "set_var"
    )
}

struct Scanner {
    file: PathBuf,
    module_dir: PathBuf,
    path_dir: PathBuf,
    failures: Vec<String>,
}

impl Scanner {
    fn scan_file(&mut self, path: &Path, module_dir: PathBuf) {
        // The fork detector owns the runtime thread it protects (design 050).
        if path.ends_with("aimdb-sync/src/fork.rs") {
            return;
        }
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                self.failures.push(format!("{}: {error}", path.display()));
                return;
            }
        };
        let file = match syn::parse_file(&source) {
            Ok(file) => file,
            Err(error) => {
                self.failures.push(format!("{}: {error}", path.display()));
                return;
            }
        };
        let previous_file = std::mem::replace(&mut self.file, path.to_owned());
        let previous_dir = std::mem::replace(&mut self.module_dir, module_dir);
        let previous_path_dir =
            std::mem::replace(&mut self.path_dir, path.parent().unwrap().to_owned());
        if !excluded(&file.attrs) {
            self.visit_file(&file);
        }
        self.file = previous_file;
        self.module_dir = previous_dir;
        self.path_dir = previous_path_dir;
    }

    fn tokens(&mut self, tokens: TokenStream) {
        for token in tokens {
            match token {
                TokenTree::Ident(ident) if forbidden(&ident.to_string()) => {
                    self.failures.push(format!(
                        "{}:{}: {ident}",
                        self.file.display(),
                        ident.span().start().line
                    ));
                }
                TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Scanner {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_test_only(item) {
            visit::visit_item(self, item);
        }
    }
    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        if !impl_item_test_only(item) {
            visit::visit_impl_item(self, item);
        }
    }
    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        if !trait_item_test_only(item) {
            visit::visit_trait_item(self, item);
        }
    }
    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if !expr_test_only(expr) {
            visit::visit_expr(self, expr);
        }
    }
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if !excluded(&local.attrs) {
            visit::visit_local(self, local);
        }
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.tokens(call.method.to_token_stream());
        visit::visit_expr_method_call(self, call);
    }
    fn visit_attribute(&mut self, _: &'ast Attribute) {}
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.tokens(item.to_token_stream());
    }
    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.tokens(path.to_token_stream());
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // Macro bodies are opaque syntax: conservatively scan their tokens.
        // Includes could hide source from this guard, so require explicit support.
        if mac.path.is_ident("include") {
            self.failures.push(format!(
                "{}: include! requires scanner support",
                self.file.display()
            ));
        }
        self.tokens(mac.to_token_stream());
    }
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if excluded(&module.attrs) {
            return;
        }
        let child_dir = self.module_dir.join(module.ident.to_string());
        if let Some((_, items)) = &module.content {
            let previous_path = std::mem::replace(&mut self.path_dir, child_dir.clone());
            let previous = std::mem::replace(&mut self.module_dir, child_dir);
            for item in items {
                self.visit_item(item);
            }
            self.module_dir = previous;
            self.path_dir = previous_path;
        } else {
            let explicit = module.attrs.iter().find_map(|attr| {
                if !attr.path().is_ident("path") {
                    return None;
                }
                if let Meta::NameValue(value) = &attr.meta {
                    if let syn::Expr::Lit(expr) = &value.value {
                        if let syn::Lit::Str(path) = &expr.lit {
                            return Some(path.value());
                        }
                    }
                }
                None
            });
            let has_path = explicit.is_some();
            let path = explicit
                .map(|path| self.path_dir.join(path))
                .unwrap_or_else(|| {
                    let flat = child_dir.with_extension("rs");
                    if flat.exists() {
                        flat
                    } else {
                        child_dir.join("mod.rs")
                    }
                });
            // rustc treats a `#[path]` file like `mod.rs`: its own `mod x;`
            // resolve next to it, whatever the file is called.
            let nested_dir = if has_path || path.file_name().is_some_and(|name| name == "mod.rs") {
                path.parent().unwrap().to_owned()
            } else {
                path.with_extension("")
            };
            self.scan_file(&path, nested_dir);
        }
    }
}

fn main() -> std::process::ExitCode {
    let roots: Vec<_> = env::args().skip(1).collect();
    if roots.is_empty() {
        eprintln!("usage: check-no-globals <crate/src/lib.rs> ...");
        return std::process::ExitCode::FAILURE;
    }
    let mut scanner = Scanner {
        file: PathBuf::new(),
        module_dir: PathBuf::new(),
        path_dir: PathBuf::new(),
        failures: vec![],
    };
    for root in roots {
        let path = Path::new(&root);
        scanner.scan_file(path, path.parent().unwrap().to_owned());
    }
    for failure in &scanner.failures {
        eprintln!("{failure}");
    }
    if scanner.failures.is_empty() {
        println!("No production library code installs a process-global");
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests;
