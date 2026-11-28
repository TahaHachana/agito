//! `#[agito::test]` and `#[agito::plugin]`. Use them through the `agito` crate.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Expr, FnArg, ItemFn, ItemStruct, Lit, Meta, Pat, Token, Type, parse_macro_input};

/// Turns an `async fn` into a browser test run by `cargo test`.
///
/// Parameters are filled by type: `Page`, `BrowserContext` and `Browser`. Each test gets a
/// fresh context and page in a browser shared by the test binary. Return `()` or a
/// `Result<(), E>`; a test that fails because the browser was lost is re-run.
///
/// Options: `retries = N` (re-runs after a lost browser, default 1),
/// `context = path::to_fn` (a `fn() -> agito::ContextOptions`), and
/// `quarantine = "reason"` (the test runs, but its failure is reported rather than failing),
/// and `perf_budget(lcp = 2500, cls = 0.1, ...)` (the budget `to_meet_performance_budget`
/// holds the test to: `lcp`, `fcp`, `ttfb`, `cls`, `inp`, `js_heap_size`, `dom_nodes`).
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    match expand_test(attr.into(), input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_test(
    attr: proc_macro2::TokenStream,
    input: ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    if input.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            input.sig.fn_token,
            "#[agito::test] functions must be async",
        ));
    }
    let args = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(attr)?;
    let mut retries = quote! { 1u32 };
    let mut context = quote! { ::agito::ContextOptions::default };
    let mut quarantine = quote! { ::core::option::Option::None };
    let mut perf_budget = quote! { ::core::option::Option::None };
    for arg in args {
        if let Meta::List(list) = &arg
            && list.path.is_ident("perf_budget")
        {
            perf_budget = parse_perf_budget(list)?;
            continue;
        }
        let Meta::NameValue(nv) = &arg else {
            return Err(syn::Error::new_spanned(
                arg,
                "expected `retries = N`, `context = path`, `quarantine = \"reason\"` or \
                 `perf_budget(...)`",
            ));
        };
        if nv.path.is_ident("retries") {
            match &nv.value {
                Expr::Lit(lit) if matches!(lit.lit, Lit::Int(_)) => {
                    let value = &lit.lit;
                    retries = quote! { #value };
                }
                other => return Err(syn::Error::new_spanned(other, "retries must be an integer")),
            }
        } else if nv.path.is_ident("context") {
            let value = &nv.value;
            context = quote! { #value };
        } else if nv.path.is_ident("quarantine") {
            match &nv.value {
                Expr::Lit(lit) if matches!(lit.lit, Lit::Str(_)) => {
                    let reason = &lit.lit;
                    quarantine = quote! { ::core::option::Option::Some(#reason) };
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "quarantine takes the reason as a string",
                    ));
                }
            }
        } else {
            return Err(syn::Error::new_spanned(
                &nv.path,
                "unknown option; expected `retries`, `context`, `quarantine` or `perf_budget`",
            ));
        }
    }

    let mut bindings = Vec::new();
    let mut call_args = Vec::new();
    for (i, input_arg) in input.sig.inputs.iter().enumerate() {
        let FnArg::Typed(pat_type) = input_arg else {
            return Err(syn::Error::new_spanned(
                input_arg,
                "#[agito::test] functions cannot take self",
            ));
        };
        let fixture = fixture_for(&pat_type.ty).ok_or_else(|| {
            syn::Error::new_spanned(
                &pat_type.ty,
                "parameters must be agito::Page, agito::BrowserContext or agito::Browser",
            )
        })?;
        let name = match &*pat_type.pat {
            Pat::Ident(p) => p.ident.clone(),
            _ => format_ident!("__agito_arg{}", i),
        };
        let field = format_ident!("{}", fixture);
        bindings.push(quote! { let #name = __agito_fixtures.#field.clone(); });
        call_args.push(name);
    }

    let attrs = &input.attrs;
    let vis = &input.vis;
    let name = &input.sig.ident;
    let mut inner = input.clone();
    inner.sig.ident = format_ident!("__agito_test_body");
    inner.attrs.clear();
    inner.vis = syn::Visibility::Inherited;

    Ok(quote! {
        #[::core::prelude::v1::test]
        #(#attrs)*
        #vis fn #name() {
            #inner
            ::agito::harness::run(
                concat!(module_path!(), "::", stringify!(#name)),
                ::agito::harness::TestOptions {
                    retries: #retries,
                    context: #context,
                    quarantine: #quarantine,
                    perf_budget: #perf_budget,
                },
                |__agito_fixtures: ::agito::harness::Fixtures| async move {
                    #(#bindings)*
                    __agito_test_body(#(#call_args),*).await
                },
            );
        }
    })
}

fn fixture_for(ty: &Type) -> Option<&'static str> {
    let Type::Path(path) = ty else { return None };
    match path.path.segments.last()?.ident.to_string().as_str() {
        "Page" => Some("page"),
        "BrowserContext" => Some("context"),
        "Browser" => Some("browser"),
        _ => None,
    }
}

/// `perf_budget(lcp = 2500, cls = 0.1)`: each metric a number, times in milliseconds.
fn parse_perf_budget(list: &syn::MetaList) -> syn::Result<proc_macro2::TokenStream> {
    let entries =
        list.parse_args_with(Punctuated::<syn::MetaNameValue, Token![,]>::parse_terminated)?;
    let mut fields = Vec::new();
    for entry in entries {
        let Some(name) = entry.path.get_ident().map(ToString::to_string) else {
            return Err(syn::Error::new_spanned(
                &entry.path,
                "expected a metric name",
            ));
        };
        let Expr::Lit(syn::ExprLit { lit, .. }) = &entry.value else {
            return Err(syn::Error::new_spanned(&entry.value, "expected a number"));
        };
        let field = format_ident!("{}", name);
        let value = match (name.as_str(), lit) {
            ("lcp" | "fcp" | "ttfb" | "cls" | "inp", Lit::Int(i)) => {
                let v: f64 = i.base10_parse()?;
                quote! { #v }
            }
            ("lcp" | "fcp" | "ttfb" | "cls" | "inp", Lit::Float(f)) => {
                let v: f64 = f.base10_parse()?;
                quote! { #v }
            }
            ("js_heap_size" | "dom_nodes", Lit::Int(i)) => {
                let v: u64 = i.base10_parse()?;
                quote! { #v }
            }
            ("js_heap_size" | "dom_nodes", _) => {
                return Err(syn::Error::new_spanned(lit, "expected a whole number"));
            }
            ("lcp" | "fcp" | "ttfb" | "cls" | "inp", _) => {
                return Err(syn::Error::new_spanned(lit, "expected a number"));
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    &entry.path,
                    "unknown metric; expected lcp, fcp, ttfb, cls, inp, js_heap_size or dom_nodes",
                ));
            }
        };
        fields.push(quote! { #field: ::core::option::Option::Some(#value) });
    }
    Ok(quote! {
        ::core::option::Option::Some(::agito::performance::PerformanceBudget {
            #(#fields,)*
            ..::agito::performance::PerformanceBudget::none()
        })
    })
}

/// Registers a plugin type so every context loads it. The type must implement
/// `agito::plugin::Plugin` and `Default`.
#[proc_macro_attribute]
pub fn plugin(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[agito::plugin] takes no arguments",
        )
        .to_compile_error()
        .into();
    }
    let input = parse_macro_input!(item as ItemStruct);
    if !input.generics.params.is_empty() {
        return syn::Error::new_spanned(
            &input.generics,
            "#[agito::plugin] types cannot be generic",
        )
        .to_compile_error()
        .into();
    }
    let name = &input.ident;
    quote! {
        #input
        ::agito::__private::inventory::submit! {
            ::agito::plugin::PluginRegistration {
                factory: || ::std::sync::Arc::new(<#name as ::core::default::Default>::default()),
            }
        }
    }
    .into()
}
