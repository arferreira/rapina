//! Metadata extraction for route handlers: request and response body schemas,
//! doc descriptions, and the `#[errors]`, `#[cache]` and `#[public]` attributes.

use quote::quote;

/// Extracts the inner type from Json<T> wrapper for schema generation
pub(crate) fn extract_json_inner_type(return_type: &syn::Type) -> Option<proc_macro2::TokenStream> {
    if let syn::Type::Path(type_path) = return_type
        && let Some(last_segment) = type_path.path.segments.last()
    {
        // Direct Json<T>
        if last_segment.ident == "Json"
            && let syn::PathArguments::AngleBracketed(args) = &last_segment.arguments
            && let Some(syn::GenericArgument::Type(inner_type)) = args.args.first()
        {
            return Some(quote!(#inner_type));
        }

        // Result<Json<T>> or Result<Json<T>, E>
        if last_segment.ident == "Result"
            && let syn::PathArguments::AngleBracketed(args) = &last_segment.arguments
            && let Some(syn::GenericArgument::Type(ok_type)) = args.args.first()
        {
            return extract_json_inner_type(ok_type);
        }
    }
    None
}

/// Extracts the request body metadata from handler function arguments.
/// Supports Json<T>, Form<T>, Validated<Json<T>>, and Validated<Form<T>>.
pub(crate) fn extract_request_body_meta(
    inputs: &syn::punctuated::Punctuated<syn::FnArg, syn::Token![,]>,
) -> Option<RequestBodyMeta> {
    for arg in inputs.iter() {
        if let syn::FnArg::Typed(pat_type) = arg {
            if let Some(meta) = extract_body_inner_type(&pat_type.ty) {
                return Some(meta);
            }
        }
    }
    None
}

/// Information about a request body extractor.
pub(crate) struct RequestBodyMeta {
    pub(crate) inner_type: proc_macro2::TokenStream,
    pub(crate) content_type: &'static str,
    pub(crate) required: bool,
}

/// Extracts the inner type and content type from Json<T>, Form<T>, Validated<Json<T>>/Validated<Form<T>>,
/// or Option<Json<T>>/Option<Form<T>>.
fn extract_body_inner_type(ty: &syn::Type) -> Option<RequestBodyMeta> {
    if let syn::Type::Path(type_path) = ty
        && let Some(last_segment) = type_path.path.segments.last()
    {
        // Direct Json<T>
        if last_segment.ident == "Json"
            && let syn::PathArguments::AngleBracketed(args) = &last_segment.arguments
            && let Some(syn::GenericArgument::Type(inner_type)) = args.args.first()
        {
            return Some(RequestBodyMeta {
                inner_type: quote!(#inner_type),
                content_type: "application/json",
                required: true,
            });
        }
        // Direct Form<T>
        if last_segment.ident == "Form"
            && let syn::PathArguments::AngleBracketed(args) = &last_segment.arguments
            && let Some(syn::GenericArgument::Type(inner_type)) = args.args.first()
        {
            return Some(RequestBodyMeta {
                inner_type: quote!(#inner_type),
                content_type: "application/x-www-form-urlencoded",
                required: true,
            });
        }
        // Validated<Json<T>> or Validated<Form<T>>
        if last_segment.ident == "Validated"
            && let syn::PathArguments::AngleBracketed(args) = &last_segment.arguments
            && let Some(syn::GenericArgument::Type(inner_extractor)) = args.args.first()
        {
            return extract_body_inner_type(inner_extractor);
        }
        // Option<Json<T>> or Option<Form<T>> - optional request body
        if last_segment.ident == "Option"
            && let syn::PathArguments::AngleBracketed(args) = &last_segment.arguments
            && let Some(syn::GenericArgument::Type(inner_extractor)) = args.args.first()
        {
            if let Some(mut meta) = extract_body_inner_type(inner_extractor) {
                meta.required = false;
                return Some(meta);
            }
        }
    }
    None
}

/// Extract the first non-empty line from `///` doc comments on a function.
pub(crate) fn extract_doc_description(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            {
                let line = s.value();
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

/// Extract #[errors(ErrorType)] attribute from function attributes, removing it if found.
pub(crate) fn extract_errors_attr(attrs: &mut Vec<syn::Attribute>) -> Option<syn::Type> {
    let idx = attrs
        .iter()
        .position(|attr| attr.path().is_ident("errors"))?;
    let attr = attrs.remove(idx);
    let err_type: syn::Type = attr.parse_args().expect("expected #[errors(ErrorType)]");
    Some(err_type)
}

/// Extract #[cache(ttl = N)] attribute from function attributes, removing it if found.
pub(crate) fn extract_cache_attr(attrs: &mut Vec<syn::Attribute>) -> Option<u64> {
    let idx = attrs
        .iter()
        .position(|attr| attr.path().is_ident("cache"))?;
    let attr = attrs.remove(idx);

    let mut ttl: Option<u64> = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("ttl") {
            let value = meta.value()?;
            let lit: syn::LitInt = value.parse()?;
            ttl = Some(lit.base10_parse()?);
            Ok(())
        } else {
            Err(meta.error("expected `ttl`"))
        }
    })
    .expect("expected #[cache(ttl = N)]");

    ttl
}

/// Removes every `#[public]` / `#[rapina::public]` attribute, returning whether any was found.
pub(crate) fn extract_public_attr(attrs: &mut Vec<syn::Attribute>) -> bool {
    let before = attrs.len();
    attrs.retain(|attr| {
        if !is_public_path(attr.path()) {
            return true;
        }
        let moved = matches!(&attr.meta, syn::Meta::List(list)
            if list.parse_args::<syn::Ident>().is_ok_and(|ident| ident == "__moved"));
        assert!(
            moved || matches!(attr.meta, syn::Meta::Path(_)),
            "#[public] takes no arguments"
        );
        false
    });
    attrs.len() != before
}

fn is_public_path(path: &syn::Path) -> bool {
    let mut segments = path.segments.iter().map(|s| &s.ident);
    match (segments.next(), segments.next(), segments.next()) {
        (Some(a), None, None) => a == "public",
        (Some(a), Some(b), None) => a == "rapina" && b == "public",
        _ => false,
    }
}
