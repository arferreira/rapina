//! The route attribute macros (`#[get]`, `#[post]`, `#[put]`, `#[patch]`, `#[delete]`).

use proc_macro::TokenStream;
use quote::quote;
use syn::{FnArg, ItemFn, LitStr};

mod headers;
mod metadata;

use headers::{HeaderParamMeta, collect_header_params, detect_header_type, gen_header_extraction};
use metadata::{
    extract_cache_attr, extract_doc_description, extract_errors_attr, extract_json_inner_type,
    extract_public_attr, extract_request_body_meta,
};

/// Parsed route macro attribute: `"/path"`, `"/path", group = "/prefix"`,
/// `"/path", description = "..."`, or any combination thereof.
struct RouteAttr {
    path: LitStr,
    group: Option<LitStr>,
    description: Option<LitStr>,
}

impl syn::parse::Parse for RouteAttr {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let path: LitStr = input.parse()?;
        let mut group: Option<LitStr> = None;
        let mut description: Option<LitStr> = None;

        while input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let ident: syn::Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            if ident == "group" {
                let value: LitStr = input.parse()?;
                group = Some(value);
            } else if ident == "description" {
                let value: LitStr = input.parse()?;
                description = Some(value);
            } else {
                return Err(syn::Error::new(
                    ident.span(),
                    "expected `group` or `description`",
                ));
            }
        }

        if !input.is_empty() {
            return Err(input.error("unexpected tokens after route attribute"));
        }
        Ok(RouteAttr {
            path,
            group,
            description,
        })
    }
}

/// Join a group prefix with a route path at compile time.
fn join_paths(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    if path.is_empty() || path == "/" {
        if prefix.is_empty() {
            return "/".to_string();
        }
        return prefix.to_string();
    }
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!("{prefix}{path}")
}

pub(crate) fn route_macro(method: &str, attr: TokenStream, item: TokenStream) -> TokenStream {
    route_macro_core(method, attr.into(), item.into()).into()
}

fn route_macro_core(
    method: &str,
    attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let route_attr: RouteAttr = syn::parse2(attr).expect("expected path as string literal");
    let path_str = if let Some(ref group) = route_attr.group {
        let g = group.value();
        assert!(
            g.starts_with('/'),
            "group prefix must start with `/`, got: {g:?}"
        );
        join_paths(&g, &route_attr.path.value())
    } else {
        route_attr.path.value()
    };
    let mut func: ItemFn = syn::parse2(item).expect("expected function");

    let func_name = &func.sig.ident;
    let func_name_str = func_name.to_string();
    let func_vis = &func.vis;

    let is_public = extract_public_attr(&mut func.attrs);

    // Resolve description: explicit attr wins, then first rustdoc line, then None
    let description_value: Option<String> = route_attr
        .description
        .as_ref()
        .map(|l| l.value())
        .or_else(|| extract_doc_description(&func.attrs));

    // Extract #[errors(ErrorType)] attribute if present
    let error_type = extract_errors_attr(&mut func.attrs);

    // Extract #[cache(ttl = N)] attribute if present
    let cache_ttl = extract_cache_attr(&mut func.attrs);

    let error_responses_impl = if let Some(err_type) = &error_type {
        quote! {
            fn error_responses() -> Vec<rapina::error::ErrorVariant> {
                <#err_type as rapina::error::DocumentedError>::error_variants()
            }
        }
    } else {
        quote! {}
    };

    // Extract return type for schema generation
    let response_schema_impl = if let syn::ReturnType::Type(_, return_type) = &func.sig.output {
        if let Some(inner_type) = extract_json_inner_type(return_type) {
            quote! {
                fn response_schema() -> Option<serde_json::Value> {
                    Some(rapina::openapi_schema_for::<#inner_type>())
                }
            }
        } else {
            quote! {}
        }
    } else {
        quote! {}
    };

    // Extract request body type and content type for schema generation.
    // Only generate requestBody for POST, PUT, and PATCH methods per OpenAPI spec.
    let (request_schema_impl, request_content_type_impl, request_body_required_impl) =
        if matches!(method, "POST" | "PUT" | "PATCH") {
            if let Some(meta) = extract_request_body_meta(&func.sig.inputs) {
                let inner_type = meta.inner_type;
                let content_type = meta.content_type;
                let required = meta.required;
                (
                    quote! {
                        fn request_schema() -> Option<serde_json::Value> {
                            Some(rapina::openapi_schema_for::<#inner_type>())
                        }
                    },
                    quote! {
                        fn request_content_type() -> Option<&'static str> {
                            Some(#content_type)
                        }
                    },
                    quote! {
                        fn request_body_required() -> Option<bool> {
                            Some(#required)
                        }
                    },
                )
            } else {
                (quote! {}, quote! {}, quote! {})
            }
        } else {
            (quote! {}, quote! {}, quote! {})
        };

    // Collect header params (also strips #[header("name")] attrs from inputs)
    let header_params = match collect_header_params(&mut func.sig.inputs) {
        Ok(p) => p,
        Err(e) => return e.to_compile_error(),
    };

    // Build an index: arg_idx → &HeaderParamMeta for O(1) lookup during codegen
    let header_by_arg: std::collections::HashMap<usize, &HeaderParamMeta> =
        header_params.iter().map(|p| (p.arg_idx, p)).collect();

    // Build header_parameters() impl for the Handler trait
    let header_parameters_impl = if header_params.is_empty() {
        quote! {}
    } else {
        let entries = header_params.iter().map(|p| {
            let name = &p.name;
            let required = p.required;
            quote! {
                rapina::discovery::HeaderParamInfo {
                    name: #name.to_string(),
                    required: #required,
                }
            }
        });
        quote! {
            fn header_parameters() -> Vec<rapina::discovery::HeaderParamInfo> {
                vec![#(#entries),*]
            }
        }
    };

    // Build description() impl for the Handler trait
    let description_impl = if let Some(ref desc) = description_value {
        quote! {
            fn description() -> Option<&'static str> {
                Some(#desc)
            }
        }
    } else {
        quote! {}
    };

    let args: Vec<_> = func.sig.inputs.iter().collect();

    // Extract return type for type annotation (helps with type inference in async blocks)
    let return_type_annotation = match &func.sig.output {
        syn::ReturnType::Type(_, ty) => quote! { : #ty },
        syn::ReturnType::Default => quote! {},
    };

    // Optional cache TTL header injection
    let cache_header_injection = if let Some(ttl) = cache_ttl {
        let ttl_str = ttl.to_string();
        quote! {
            let mut __rapina_response = __rapina_response;
            __rapina_response.headers_mut().insert(
                "x-rapina-cache-ttl",
                rapina::http::HeaderValue::from_static(#ttl_str),
            );
        }
    } else {
        quote! {}
    };

    // Build the handler body
    // Use __rapina_ prefix for internal variables to avoid shadowing user's variables
    let handler_body = if args.is_empty() {
        let inner_block = &func.block;
        quote! {
            let __rapina_result #return_type_annotation = (async #inner_block).await;
            let __rapina_response = rapina::response::IntoResponse::into_response(__rapina_result);
            #cache_header_injection
            __rapina_response
        }
    } else {
        let inner_block = &func.block;

        // Check if all args are header extractors (so we never need to split req into parts)
        let all_headers = args.iter().all(|arg| {
            if let FnArg::Typed(pt) = arg {
                detect_header_type(&pt.ty).is_some()
            } else {
                false
            }
        });

        // Check if the single arg is a header type
        let single_is_header = args.len() == 1
            && args.first().is_some_and(|arg| {
                if let FnArg::Typed(pt) = arg {
                    detect_header_type(&pt.ty).is_some()
                } else {
                    false
                }
            });

        if args.len() == 1 && !single_is_header {
            // Single non-header arg: pass request directly to FromRequest
            let arg = &args[0];
            if let FnArg::Typed(pat_type) = arg {
                let pat = &pat_type.pat;
                let arg_type = &pat_type.ty;
                let tmp = syn::Ident::new("__rapina_arg_0", proc_macro2::Span::call_site());
                quote! {
                    let #tmp = match <#arg_type as rapina::extract::FromRequest>::from_request(__rapina_req, &__rapina_params, &__rapina_state).await {
                        Ok(v) => v,
                        Err(e) => return rapina::response::IntoResponse::into_response(e),
                    };
                    let #pat = #tmp;
                    let __rapina_result #return_type_annotation = (async #inner_block).await;
                    let __rapina_response = rapina::response::IntoResponse::into_response(__rapina_result);
                    #cache_header_injection
                    __rapina_response
                }
            } else {
                unreachable!("handler argument must be a typed pattern")
            }
        } else if all_headers {
            // All args are header extractors — extract from parts, no body split needed
            let mut header_extractions = Vec::new();
            for (i, arg) in args.iter().enumerate() {
                if let FnArg::Typed(pat_type) = arg {
                    let pat = &pat_type.pat;
                    let tmp = syn::Ident::new(
                        &format!("__rapina_arg_{}", i),
                        proc_macro2::Span::call_site(),
                    );
                    let meta = header_by_arg.get(&i).expect("all_headers: missing meta");
                    header_extractions.push(gen_header_extraction(
                        &meta.inner_type,
                        meta.required,
                        &meta.name,
                        &tmp,
                    ));
                    header_extractions.push(quote! { let #pat = #tmp; });
                }
            }
            quote! {
                let (__rapina_parts, _) = __rapina_req.into_parts();
                #(#header_extractions)*
                let __rapina_result #return_type_annotation = (async #inner_block).await;
                let __rapina_response = rapina::response::IntoResponse::into_response(__rapina_result);
                #cache_header_injection
                __rapina_response
            }
        } else {
            // Multiple args: all but last use FromRequestParts (or header extraction), last uses FromRequest
            let mut parts_extractions = Vec::new();

            for (i, arg) in args[..args.len() - 1].iter().enumerate() {
                if let FnArg::Typed(pat_type) = arg {
                    let pat = &pat_type.pat;
                    let arg_type = &pat_type.ty;
                    let tmp = syn::Ident::new(
                        &format!("__rapina_arg_{}", i),
                        proc_macro2::Span::call_site(),
                    );
                    if detect_header_type(arg_type).is_some() {
                        let meta = header_by_arg.get(&i).expect("mixed: missing meta");
                        parts_extractions.push(gen_header_extraction(
                            &meta.inner_type,
                            meta.required,
                            &meta.name,
                            &tmp,
                        ));
                        parts_extractions.push(quote! { let #pat = #tmp; });
                    } else {
                        parts_extractions.push(quote! {
                            let #tmp = match <#arg_type as rapina::extract::FromRequestParts>::from_request_parts(&__rapina_parts, &__rapina_params, &__rapina_state).await {
                                Ok(v) => v,
                                Err(e) => return rapina::response::IntoResponse::into_response(e),
                            };
                            let #pat = #tmp;
                        });
                    }
                }
            }

            let last_arg = args.last().unwrap();
            let last_extraction = if let FnArg::Typed(pat_type) = last_arg {
                let pat = &pat_type.pat;
                let arg_type = &pat_type.ty;
                let last_idx = args.len() - 1;
                let tmp = syn::Ident::new(
                    &format!("__rapina_arg_{}", last_idx),
                    proc_macro2::Span::call_site(),
                );
                if detect_header_type(arg_type).is_some() {
                    let meta = header_by_arg
                        .get(&last_idx)
                        .expect("last arg: missing meta");
                    let header_extr =
                        gen_header_extraction(&meta.inner_type, meta.required, &meta.name, &tmp);
                    quote! {
                        #header_extr
                        let #pat = #tmp;
                        // Reconstruct the request (body not consumed for header-only last arg)
                        let _ = __rapina_body;
                    }
                } else {
                    quote! {
                        let __rapina_req = rapina::http::Request::from_parts(__rapina_parts, __rapina_body);
                        let #tmp = match <#arg_type as rapina::extract::FromRequest>::from_request(__rapina_req, &__rapina_params, &__rapina_state).await {
                            Ok(v) => v,
                            Err(e) => return rapina::response::IntoResponse::into_response(e),
                        };
                        let #pat = #tmp;
                    }
                }
            } else {
                unreachable!("handler argument must be a typed pattern")
            };

            quote! {
                let (__rapina_parts, __rapina_body) = __rapina_req.into_parts();
                #(#parts_extractions)*
                #last_extraction
                let __rapina_result #return_type_annotation = (async #inner_block).await;
                let __rapina_response = rapina::response::IntoResponse::into_response(__rapina_result);
                #cache_header_injection
                __rapina_response
            }
        }
    };

    // Build the router method call for the register function
    let router_method = syn::Ident::new(&method.to_lowercase(), proc_macro2::Span::call_site());
    let register_fn_name = syn::Ident::new(
        &format!("__rapina_register_{}", func_name_str),
        proc_macro2::Span::call_site(),
    );

    // Generate the struct, Handler impl, and inventory submission
    quote! {
        #[derive(Clone, Copy)]
        #[allow(non_camel_case_types)]
        #func_vis struct #func_name;

        impl rapina::handler::Handler for #func_name {
            const NAME: &'static str = #func_name_str;

            #response_schema_impl
            #request_schema_impl
            #request_content_type_impl
            #request_body_required_impl
            #error_responses_impl
            #header_parameters_impl
            #description_impl

            fn call(
                &self,
                __rapina_req: rapina::hyper::Request<rapina::hyper::body::Incoming>,
                __rapina_params: rapina::extract::PathParams,
                __rapina_state: std::sync::Arc<rapina::state::AppState>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = rapina::hyper::Response<rapina::response::BoxBody>> + Send>> {
                Box::pin(async move {
                    #handler_body
                })
            }
        }

        #[doc(hidden)]
        fn #register_fn_name(__rapina_router: rapina::router::Router) -> rapina::router::Router {
            __rapina_router.#router_method(#path_str, #func_name)
        }

        rapina::inventory::submit! {
            rapina::discovery::RouteDescriptor {
                method: #method,
                path: #path_str,
                handler_name: #func_name_str,
                is_public: #is_public,
                response_schema: <#func_name as rapina::handler::Handler>::response_schema,
                request_schema: <#func_name as rapina::handler::Handler>::request_schema,
                request_content_type: <#func_name as rapina::handler::Handler>::request_content_type,
                request_body_required: <#func_name as rapina::handler::Handler>::request_body_required,
                error_responses: <#func_name as rapina::handler::Handler>::error_responses,
                header_parameters: <#func_name as rapina::handler::Handler>::header_parameters,
                description: <#func_name as rapina::handler::Handler>::description,
                register: #register_fn_name,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn test_generates_struct_with_handler_impl() {
        let path = quote!("/");
        let input = quote! {
            async fn hello() -> &'static str {
                "Hello, Rapina!"
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        // Check struct is generated
        assert!(output_str.contains("struct hello"));
        // Check Handler impl is generated
        assert!(output_str.contains("impl rapina :: handler :: Handler for hello"));
        // Check NAME constant
        assert!(output_str.contains("const NAME"));
        assert!(output_str.contains("\"hello\""));
    }

    #[test]
    fn test_generates_handler_with_extractors() {
        let path = quote!("/users/:id");
        let input = quote! {
            async fn get_user(id: rapina::extract::Path<u64>) -> String {
                format!("{}", id.into_inner())
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("struct get_user"));
        // Single arg is last arg — uses FromRequest (blanket impl handles parts-only)
        assert!(output_str.contains("FromRequest"));
        // Single arg should NOT destructure request into parts
        assert!(!output_str.contains("into_parts"));
    }

    #[test]
    fn test_function_with_multiple_extractors() {
        let path = quote!("/users");
        let input = quote! {
            async fn create_user(
                id: rapina::extract::Path<u64>,
                body: rapina::extract::Json<String>
            ) -> String {
                "created".to_string()
            }
        };

        let output = route_macro_core("POST", path, input);
        let output_str = output.to_string();

        // Check struct is generated
        assert!(output_str.contains("struct create_user"));
        // Check both extractors are handled
        assert!(output_str.contains("FromRequestParts"));
        assert!(output_str.contains("FromRequest"));
    }

    #[test]
    fn test_two_body_extractors_no_macro_panic() {
        // With positional convention, the macro does NOT panic for multiple body consumers.
        // Instead, it generates code where the first Json is bounded by FromRequestParts
        // (which it doesn't implement), so the compiler catches it at type-check time.
        let path = quote!("/users");
        let input = quote! {
            async fn handler(
                body1: rapina::extract::Json<String>,
                body2: rapina::extract::Json<String>
            ) -> String {
                "ok".to_string()
            }
        };

        // Should NOT panic — macro expansion succeeds, compiler catches the error later
        let output = route_macro_core("POST", path, input);
        let output_str = output.to_string();

        // First arg gets FromRequestParts (will fail at compile time since Json doesn't impl it)
        assert!(output_str.contains("FromRequestParts"));
        // Last arg gets FromRequest
        assert!(output_str.contains("FromRequest"));
    }

    #[test]
    fn test_custom_type_name_not_misclassified() {
        // UserPathInfo contains "Path" but should NOT be routed to FromRequestParts
        // Positional convention: single (last) arg always uses FromRequest
        let path = quote!("/users");
        let input = quote! {
            async fn handler(info: UserPathInfo) -> String {
                "ok".to_string()
            }
        };

        let output = route_macro_core("POST", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("FromRequest"));
        assert!(!output_str.contains("FromRequestParts"));
    }

    #[test]
    fn test_multiple_parts_only_extractors_positional() {
        // All parts-only extractors: first N-1 use FromRequestParts, last uses FromRequest
        let path = quote!("/users/:id");
        let input = quote! {
            async fn handler(
                id: rapina::extract::Path<u64>,
                query: rapina::extract::Query<Params>,
                headers: rapina::extract::Headers,
            ) -> String {
                "ok".to_string()
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        // First two args use FromRequestParts
        assert!(output_str.contains("FromRequestParts"));
        // Last arg uses FromRequest (via blanket impl at runtime)
        assert!(output_str.contains("FromRequest"));
        // Request is destructured for multi-arg case
        assert!(output_str.contains("into_parts"));
        // Request is reassembled for last arg
        assert!(output_str.contains("from_parts"));
    }

    #[test]
    #[should_panic(expected = "expected function")]
    fn test_invalid_input_panics() {
        let path = quote!("/");
        let invalid_input = quote! { not_a_function };

        route_macro_core("GET", path, invalid_input);
    }

    #[test]
    fn test_json_return_type_generates_response_schema() {
        let path = quote!("/users");
        let input = quote! {
            async fn get_user() -> Json<UserResponse> {
                Json(UserResponse { id: 1 })
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        // Check response_schema method is generated with openapi_schema_for
        assert!(output_str.contains("fn response_schema"));
        assert!(output_str.contains("rapina :: openapi_schema_for"));
        assert!(output_str.contains("UserResponse"));
    }

    #[test]
    fn test_result_json_return_type_generates_response_schema() {
        let path = quote!("/users");
        let input = quote! {
            async fn get_user() -> Result<Json<UserResponse>> {
                Ok(Json(UserResponse { id: 1 }))
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("fn response_schema"));
        assert!(output_str.contains("rapina :: openapi_schema_for"));
        assert!(output_str.contains("UserResponse"));
    }

    #[test]
    fn test_errors_attr_generates_error_responses() {
        let path = quote!("/users");
        let input = quote! {
            #[errors(UserError)]
            async fn get_user() -> Result<Json<UserResponse>> {
                Ok(Json(UserResponse { id: 1 }))
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("fn error_responses"));
        assert!(output_str.contains("DocumentedError"));
        assert!(output_str.contains("UserError"));
    }

    #[test]
    fn test_json_body_generates_request_schema_and_content_type() {
        let path = quote!("/users");
        let input = quote! {
            async fn create_user(body: Json<CreateUserRequest>) -> Json<UserResponse> {
                Json(UserResponse { id: 1 })
            }
        };

        let output = route_macro_core("POST", path, input);
        let output_str = output.to_string();

        // Check request_schema method is generated
        assert!(output_str.contains("fn request_schema"));
        assert!(output_str.contains("CreateUserRequest"));
        // Check request_content_type method is generated with JSON content type
        assert!(output_str.contains("fn request_content_type"));
        assert!(output_str.contains("application/json"));
    }

    #[test]
    fn test_form_body_generates_request_schema_and_content_type() {
        let path = quote!("/users");
        let input = quote! {
            async fn create_user(body: Form<CreateUserForm>) -> Json<UserResponse> {
                Json(UserResponse { id: 1 })
            }
        };

        let output = route_macro_core("POST", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("fn request_schema"));
        assert!(output_str.contains("CreateUserForm"));
        // Check request_content_type method is generated with form content type
        assert!(output_str.contains("fn request_content_type"));
        assert!(output_str.contains("application/x-www-form-urlencoded"));
    }

    #[test]
    fn test_validated_json_generates_request_schema_and_content_type() {
        let path = quote!("/users");
        let input = quote! {
            async fn create_user(body: Validated<Json<CreateUserRequest>>) -> Json<UserResponse> {
                Json(UserResponse { id: 1 })
            }
        };

        let output = route_macro_core("POST", path, input);
        let output_str = output.to_string();

        // Should extract CreateUserRequest from Validated<Json<CreateUserRequest>>
        assert!(output_str.contains("fn request_schema"));
        assert!(output_str.contains("CreateUserRequest"));
        // Should inherit JSON content type from inner Json extractor
        assert!(output_str.contains("fn request_content_type"));
        assert!(output_str.contains("application/json"));
    }

    #[test]
    fn test_validated_form_generates_request_schema_and_content_type() {
        let path = quote!("/login");
        let input = quote! {
            async fn login(body: Validated<Form<LoginForm>>) -> Json<TokenResponse> {
                Json(TokenResponse { token: "abc".into() })
            }
        };

        let output = route_macro_core("POST", path, input);
        let output_str = output.to_string();

        // Should extract LoginForm from Validated<Form<LoginForm>>
        assert!(output_str.contains("fn request_schema"));
        assert!(output_str.contains("LoginForm"));
        // Should inherit form content type from inner Form extractor
        assert!(output_str.contains("fn request_content_type"));
        assert!(output_str.contains("application/x-www-form-urlencoded"));
    }

    #[test]
    fn test_option_json_generates_optional_request_body() {
        let path = quote!("/users");
        let input = quote! {
            async fn update_user(body: Option<Json<UpdateUserRequest>>) -> Json<UserResponse> {
                Json(UserResponse { id: 1 })
            }
        };

        let output = route_macro_core("PATCH", path, input);
        let output_str = output.to_string();

        // Should extract UpdateUserRequest from Option<Json<UpdateUserRequest>>
        assert!(output_str.contains("fn request_schema"));
        assert!(output_str.contains("UpdateUserRequest"));
        // Should have JSON content type
        assert!(output_str.contains("fn request_content_type"));
        assert!(output_str.contains("application/json"));
        // Should have request_body_required returning false
        assert!(output_str.contains("fn request_body_required"));
        assert!(output_str.contains("Some (false)"));
    }

    #[test]
    fn test_option_form_generates_optional_request_body() {
        let path = quote!("/login");
        let input = quote! {
            async fn login(body: Option<Form<LoginForm>>) -> Json<TokenResponse> {
                Json(TokenResponse { token: "abc".into() })
            }
        };

        let output = route_macro_core("POST", path, input);
        let output_str = output.to_string();

        // Should extract LoginForm from Option<Form<LoginForm>>
        assert!(output_str.contains("fn request_schema"));
        assert!(output_str.contains("LoginForm"));
        // Should have form content type
        assert!(output_str.contains("fn request_content_type"));
        assert!(output_str.contains("application/x-www-form-urlencoded"));
        // Should have request_body_required returning false
        assert!(output_str.contains("fn request_body_required"));
        assert!(output_str.contains("Some (false)"));
    }

    #[test]
    fn test_get_with_json_body_no_request_schema() {
        // GET handlers should not generate requestBody even if they have Json<T> parameter
        let path = quote!("/users");
        let input = quote! {
            async fn list_users(body: Json<FilterRequest>) -> Json<Vec<UserResponse>> {
                Json(vec![])
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        // Should NOT generate request_schema for GET method
        assert!(!output_str.contains("fn request_schema"));
        assert!(!output_str.contains("fn request_content_type"));
        assert!(!output_str.contains("fn request_body_required"));
    }

    #[test]
    fn test_delete_with_json_body_no_request_schema() {
        // DELETE handlers should not generate requestBody even if they have Json<T> parameter
        let path = quote!("/users/:id");
        let input = quote! {
            async fn delete_user(body: Json<DeleteRequest>) -> StatusCode {
                StatusCode::NO_CONTENT
            }
        };

        let output = route_macro_core("DELETE", path, input);
        let output_str = output.to_string();

        // Should NOT generate request_schema for DELETE method
        assert!(!output_str.contains("fn request_schema"));
        assert!(!output_str.contains("fn request_content_type"));
        assert!(!output_str.contains("fn request_body_required"));
    }

    #[test]
    fn test_no_body_no_request_schema_or_content_type() {
        let path = quote!("/users");
        let input = quote! {
            async fn list_users() -> Json<Vec<UserResponse>> {
                Json(vec![])
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        // Should NOT generate request_schema or request_content_type for handlers without body
        assert!(!output_str.contains("fn request_schema"));
        assert!(!output_str.contains("fn request_content_type"));
    }

    #[test]
    fn test_non_json_return_type_no_response_schema() {
        let path = quote!("/health");
        let input = quote! {
            async fn health() -> &'static str {
                "ok"
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        // Check response_schema method is NOT generated for non-Json types
        assert!(!output_str.contains("fn response_schema"));
        assert!(!output_str.contains("openapi_schema_for"));
    }

    #[test]
    fn test_user_state_variable_not_shadowed() {
        // Regression test for issue #134 - user naming their extractor 'state'
        // should not conflict with internal macro variables
        let path = quote!("/users");
        let input = quote! {
            async fn list_users(state: rapina::extract::State<MyState>) -> String {
                "ok".to_string()
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        // Internal variables should use __rapina_ prefix
        assert!(output_str.contains("__rapina_state"));
        assert!(output_str.contains("__rapina_params"));
        // User's variable 'state' should still be extracted
        assert!(output_str.contains("let state ="));
    }

    #[test]
    fn test_no_closure_wrapper_for_type_inference() {
        // Regression test for issue #134 - Result type inference should work
        let path = quote!("/users");
        let input = quote! {
            async fn get_user() -> Result<String, Error> {
                Ok("user".to_string())
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        // Should NOT use closure wrapper (|| async ...)
        assert!(!output_str.contains("|| async"));
        // Should use typed result with async block (: ReturnType = (async ...).await)
        assert!(output_str.contains("__rapina_result"));
        assert!(output_str.contains("Result < String , Error >"));
    }

    #[test]
    fn test_emits_route_descriptor() {
        let path = quote!("/users");
        let input = quote! {
            async fn list_users() -> &'static str {
                "users"
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("inventory :: submit !"));
        assert!(output_str.contains("RouteDescriptor"));
        assert!(output_str.contains("method : \"GET\""));
        assert!(output_str.contains("path : \"/users\""));
        assert!(output_str.contains("handler_name : \"list_users\""));
        assert!(output_str.contains("is_public : false"));
        assert!(output_str.contains("__rapina_register_list_users"));
    }

    #[test]
    fn test_emits_route_descriptor_with_method() {
        let path = quote!("/users");
        let input = quote! {
            async fn create_user() -> &'static str {
                "created"
            }
        };

        let output = route_macro_core("POST", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("method : \"POST\""));
        assert!(output_str.contains("__rapina_router . post"));
    }

    #[test]
    fn test_public_attr_below_route_sets_is_public() {
        let path = quote!("/health");
        let input = quote! {
            #[public]
            async fn health() -> &'static str {
                "ok"
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("is_public : true"));
    }

    #[test]
    fn test_moved_qualified_public_attr_sets_is_public_and_is_stripped() {
        let path = quote!("/health");
        let input = quote! {
            #[rapina::public(__moved)]
            async fn health() -> &'static str {
                "ok"
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("is_public : true"));
        assert!(!output_str.contains("__moved"));
    }

    #[test]
    #[should_panic(expected = "#[public] takes no arguments")]
    fn test_public_attr_below_route_rejects_arguments() {
        let path = quote!("/health");
        let input = quote! {
            #[public(false)]
            async fn health() -> &'static str {
                "ok"
            }
        };

        route_macro_core("GET", path, input);
    }

    #[test]
    fn test_cache_attr_injects_ttl_header() {
        let path = quote!("/products");
        let input = quote! {
            #[cache(ttl = 60)]
            async fn list_products() -> &'static str {
                "products"
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("x-rapina-cache-ttl"));
        assert!(output_str.contains("60"));
    }

    #[test]
    fn test_no_cache_attr_no_ttl_header() {
        let path = quote!("/products");
        let input = quote! {
            async fn list_products() -> &'static str {
                "products"
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        assert!(!output_str.contains("x-rapina-cache-ttl"));
    }

    #[test]
    fn test_cache_attr_with_extractors() {
        let path = quote!("/users/:id");
        let input = quote! {
            #[cache(ttl = 120)]
            async fn get_user(id: rapina::extract::Path<u64>) -> String {
                format!("{}", id.into_inner())
            }
        };

        let output = route_macro_core("GET", path, input);
        let output_str = output.to_string();

        assert!(output_str.contains("x-rapina-cache-ttl"));
        assert!(output_str.contains("120"));
        // Single arg uses FromRequest (positional convention)
        assert!(output_str.contains("FromRequest"));
    }

    #[test]
    fn test_group_param_joins_path() {
        let attr = quote!("/users", group = "/api");
        let input = quote! {
            async fn list_users() -> &'static str {
                "users"
            }
        };

        let output = route_macro_core("GET", attr, input);
        let output_str = output.to_string();

        assert!(output_str.contains("path : \"/api/users\""));
        assert!(output_str.contains("__rapina_router . get (\"/api/users\""));
    }

    #[test]
    fn test_group_param_with_nested_prefix() {
        let attr = quote!("/items", group = "/api/v1");
        let input = quote! {
            async fn list_items() -> &'static str {
                "items"
            }
        };

        let output = route_macro_core("GET", attr, input);
        let output_str = output.to_string();

        assert!(output_str.contains("path : \"/api/v1/items\""));
    }

    #[test]
    fn test_without_group_param_backward_compatible() {
        let attr = quote!("/users");
        let input = quote! {
            async fn list_users() -> &'static str {
                "users"
            }
        };

        let output = route_macro_core("GET", attr, input);
        let output_str = output.to_string();

        assert!(output_str.contains("path : \"/users\""));
        assert!(output_str.contains("__rapina_router . get (\"/users\""));
    }

    #[test]
    #[should_panic(expected = "group prefix must start with `/`")]
    fn test_group_prefix_must_start_with_slash() {
        let attr = quote!("/users", group = "api");
        let input = quote! {
            async fn list_users() -> &'static str {
                "users"
            }
        };

        route_macro_core("GET", attr, input);
    }

    #[test]
    fn test_group_with_trailing_slash_normalized() {
        let attr = quote!("/users", group = "/api/");
        let input = quote! {
            async fn list_users() -> &'static str {
                "users"
            }
        };

        let output = route_macro_core("GET", attr, input);
        let output_str = output.to_string();

        assert!(output_str.contains("path : \"/api/users\""));
    }

    #[test]
    fn test_group_with_public_attr() {
        let attr = quote!("/health", group = "/api");
        let input = quote! {
            #[public]
            async fn health() -> &'static str {
                "ok"
            }
        };

        let output = route_macro_core("GET", attr, input);
        let output_str = output.to_string();

        assert!(output_str.contains("path : \"/api/health\""));
        assert!(output_str.contains("is_public : true"));
    }

    #[test]
    fn test_group_with_cache_attr() {
        let attr = quote!("/products", group = "/api");
        let input = quote! {
            #[cache(ttl = 60)]
            async fn list_products() -> &'static str {
                "products"
            }
        };

        let output = route_macro_core("GET", attr, input);
        let output_str = output.to_string();

        assert!(output_str.contains("path : \"/api/products\""));
        assert!(output_str.contains("x-rapina-cache-ttl"));
        assert!(output_str.contains("60"));
    }

    #[test]
    fn test_group_with_errors_attr() {
        let attr = quote!("/users", group = "/api");
        let input = quote! {
            #[errors(UserError)]
            async fn get_user() -> Result<Json<UserResponse>> {
                Ok(Json(UserResponse { id: 1 }))
            }
        };

        let output = route_macro_core("GET", attr, input);
        let output_str = output.to_string();

        assert!(output_str.contains("path : \"/api/users\""));
        assert!(output_str.contains("fn error_responses"));
        assert!(output_str.contains("UserError"));
    }

    #[test]
    fn test_group_with_all_methods() {
        for method in &["GET", "POST", "PUT", "DELETE"] {
            let attr = quote!("/items", group = "/api");
            let input = quote! {
                async fn handler() -> &'static str {
                    "ok"
                }
            };

            let output = route_macro_core(method, attr, input);
            let output_str = output.to_string();

            assert!(
                output_str.contains("path : \"/api/items\""),
                "{method} should produce /api/items"
            );
            let method_lower = method.to_lowercase();
            assert!(
                output_str.contains(&format!("__rapina_router . {method_lower}")),
                "{method} should use .{method_lower}() on router"
            );
        }
    }

    #[test]
    fn test_join_paths_basic() {
        assert_eq!(join_paths("/api", "/users"), "/api/users");
        assert_eq!(join_paths("/api/v1", "/items"), "/api/v1/items");
    }

    #[test]
    fn test_join_paths_trailing_slash() {
        assert_eq!(join_paths("/api/", "/users"), "/api/users");
    }

    #[test]
    fn test_join_paths_empty_path() {
        assert_eq!(join_paths("/api", ""), "/api");
        assert_eq!(join_paths("/api", "/"), "/api");
    }

    #[test]
    fn test_join_paths_empty_prefix() {
        assert_eq!(join_paths("", "/users"), "/users");
        assert_eq!(join_paths("", ""), "/");
    }
}
