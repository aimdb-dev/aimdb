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

// Extract outer attributes uniformly from items, impl members and expressions.
fn test_only(node: &impl ToTokens) -> bool {
    let parser = |input: syn::parse::ParseStream<'_>| {
        let attrs = input.call(Attribute::parse_outer)?;
        let _: TokenStream = input.parse()?;
        Ok(attrs)
    };
    parser
        .parse2(node.to_token_stream())
        .is_ok_and(|attrs| excluded(&attrs))
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
        if !test_only(item) {
            visit::visit_item(self, item);
        }
    }
    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        if !test_only(item) {
            visit::visit_impl_item(self, item);
        }
    }
    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        if !test_only(item) {
            visit::visit_trait_item(self, item);
        }
    }
    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if !test_only(expr) {
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
            let nested_dir = if path.file_name().is_some_and(|name| name == "mod.rs") {
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
