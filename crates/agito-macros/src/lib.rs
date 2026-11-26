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
/// Options: `retries = N` (re-runs after a lost browser, default 1) and
/// `context = path::to_fn` (a `fn() -> agito::ContextOptions`).
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
    for arg in args {
        let Meta::NameValue(nv) = &arg else {
            return Err(syn::Error::new_spanned(
                arg,
                "expected `retries = N` or `context = path`",
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
        } else {
            return Err(syn::Error::new_spanned(
                &nv.path,
                "unknown option; expected `retries` or `context`",
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
                ::agito::harness::TestOptions { retries: #retries, context: #context },
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
