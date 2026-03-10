use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Type;

use crate::parse::{EndpointDef, HttpMethod, ParamDef, ServiceDef};

pub fn generate(def: ServiceDef) -> TokenStream {
    let service_name = &def.name;
    let trait_name = format_ident!("{}Service", service_name);
    let routes_mod = format_ident!("{}_routes", to_snake_case(&service_name.to_string()));

    // 1. Generate route constants
    let route_consts = generate_route_consts(&def.endpoints);

    // 2. Generate service trait methods
    let trait_methods = generate_trait_methods(&def.endpoints);

    // 3. Generate handler functions (top-level, outside impl block)
    let handler_fns: Vec<TokenStream> = def
        .endpoints
        .iter()
        .map(|ep| generate_handler(ep, &trait_name))
        .collect();

    // 4. Generate route registrations (grouped by path for same-path multi-method)
    let route_calls = generate_grouped_routes(&def.endpoints, &routes_mod);

    quote! {
        /// Route path constants (auto-generated)
        #[allow(dead_code)]
        pub mod #routes_mod {
            #(#route_consts)*
        }

        /// Service trait (auto-generated). Implement this for your application state.
        pub trait #trait_name: Send + Sync + 'static {
            #(#trait_methods)*
        }

        // Handler functions (auto-generated, not part of public API)
        #(#handler_fns)*

        /// Service definition (auto-generated).
        pub struct #service_name;

        impl #service_name {
            /// Build an axum Router with the given state type.
            ///
            /// Returns `Router<Arc<S>>` — the caller is responsible for
            /// calling `.with_state(state)` when ready.
            pub fn router<S: #trait_name>() -> ::axum::Router<::std::sync::Arc<S>> {
                ::axum::Router::new()
                    #(#route_calls)*
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Route constants
// ---------------------------------------------------------------------------

fn generate_route_consts(endpoints: &[EndpointDef]) -> Vec<TokenStream> {
    endpoints
        .iter()
        .map(|ep| {
            let const_name = format_ident!("{}", ep.fn_name.to_string().to_uppercase());
            let axum_path = format!("/{}", &ep.path);
            quote! {
                pub const #const_name: &str = #axum_path;
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Trait methods
// ---------------------------------------------------------------------------

fn generate_trait_methods(endpoints: &[EndpointDef]) -> Vec<TokenStream> {
    endpoints
        .iter()
        .map(|ep| {
            let fn_name = &ep.fn_name;
            let params = trait_params(&ep.params);
            let ret = match &ep.return_type {
                Some(ty) => {
                    let qualified = qualify_known_types(ty);
                    quote! { ::std::result::Result<#qualified, ::mad_hatter::HttpError> }
                }
                None => quote! { ::std::result::Result<(), ::mad_hatter::HttpError> },
            };
            quote! {
                fn #fn_name(&self, #(#params),*) -> impl ::std::future::Future<Output = #ret> + Send;
            }
        })
        .collect()
}

fn trait_params(params: &[ParamDef]) -> Vec<TokenStream> {
    params
        .iter()
        .map(|p| {
            let name = &p.name;
            let ty = &p.ty;
            quote! { #name: #ty }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Handler functions
// ---------------------------------------------------------------------------

fn generate_handler(ep: &EndpointDef, trait_name: &proc_macro2::Ident) -> TokenStream {
    let handler_name = format_ident!("__handle_{}", ep.fn_name);
    let fn_name = &ep.fn_name;

    // Classify parameters: path params, query param, body param
    let path_param_names = extract_path_param_names(&ep.path);

    let path_params: Vec<&ParamDef> = ep
        .params
        .iter()
        .filter(|p| path_param_names.contains(&p.name.to_string()))
        .collect();

    let query_param: Option<&ParamDef> = ep.params.iter().find(|p| p.name == "query");
    let body_param: Option<&ParamDef> = ep.params.iter().find(|p| p.name == "body");

    // Build extractor parameters for the handler
    let mut handler_params = Vec::new();
    let mut call_args = Vec::new();

    // State is always first
    handler_params.push(quote! {
        ::axum::extract::State(svc): ::axum::extract::State<::std::sync::Arc<S>>
    });

    // Path parameters
    if path_params.len() == 1 {
        let ty = &path_params[0].ty;
        let name = &path_params[0].name;
        handler_params.push(quote! {
            ::axum::extract::Path(#name): ::axum::extract::Path<#ty>
        });
        call_args.push(quote! { #name });
    } else if path_params.len() > 1 {
        let types: Vec<&Type> = path_params.iter().map(|p| &p.ty).collect();
        let names: Vec<&proc_macro2::Ident> = path_params.iter().map(|p| &p.name).collect();
        let tuple_names = quote! { (#(#names),*) };
        let tuple_types = quote! { (#(#types),*) };
        handler_params.push(quote! {
            ::axum::extract::Path(#tuple_names): ::axum::extract::Path<#tuple_types>
        });
        for name in &names {
            call_args.push(quote! { #name });
        }
    }

    // Query parameter (must come after Path in axum extractors)
    if let Some(qp) = query_param {
        let ty = &qp.ty;
        handler_params.push(quote! {
            ::axum::extract::Query(query): ::axum::extract::Query<#ty>
        });
        call_args.push(quote! { query });
    }

    // Body parameter — Json<T> or raw String
    if let Some(bp) = body_param {
        let ty = &bp.ty;
        if is_string_type(ty) {
            handler_params.push(quote! {
                body: ::std::string::String
            });
        } else {
            handler_params.push(quote! {
                ::axum::Json(body): ::axum::Json<#ty>
            });
        }
        call_args.push(quote! { body });
    }

    // Return type handling
    let response_conversion = if ep.return_type.is_some() {
        quote! {
            match svc.#fn_name(#(#call_args),*).await {
                Ok(val) => ::axum::response::IntoResponse::into_response(val),
                Err(err) => ::axum::response::IntoResponse::into_response(err),
            }
        }
    } else {
        quote! {
            match svc.#fn_name(#(#call_args),*).await {
                Ok(()) => ::axum::response::IntoResponse::into_response(::axum::http::StatusCode::NO_CONTENT),
                Err(err) => ::axum::response::IntoResponse::into_response(err),
            }
        }
    };

    quote! {
        async fn #handler_name<S: #trait_name>(
            #(#handler_params),*
        ) -> ::axum::response::Response {
            #response_conversion
        }
    }
}

// ---------------------------------------------------------------------------
// Route registration (grouped by path for same-path multi-method support)
// ---------------------------------------------------------------------------

fn generate_grouped_routes(
    endpoints: &[EndpointDef],
    routes_mod: &proc_macro2::Ident,
) -> Vec<TokenStream> {
    // Group endpoints by path, preserving insertion order
    let mut groups: Vec<(String, Vec<&EndpointDef>)> = Vec::new();
    let mut path_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for ep in endpoints {
        if let Some(&idx) = path_index.get(&ep.path) {
            groups[idx].1.push(ep);
        } else {
            path_index.insert(ep.path.clone(), groups.len());
            groups.push((ep.path.clone(), vec![ep]));
        }
    }

    groups
        .iter()
        .map(|(_path, eps)| {
            // Use the first endpoint's const name for the route path reference
            let first_const = format_ident!("{}", eps[0].fn_name.to_string().to_uppercase());

            // Build method router chain: get(h1).post(h2)...
            let first = &eps[0];
            let first_handler = format_ident!("__handle_{}", first.fn_name);
            let first_routing_fn = method_routing_fn(&first.method);

            let mut chain = quote! { #first_routing_fn(#first_handler::<S>) };

            for ep in &eps[1..] {
                let handler = format_ident!("__handle_{}", ep.fn_name);
                let method_name = method_name_ident(&ep.method);
                chain = quote! { #chain.#method_name(#handler::<S>) };
            }

            quote! {
                .route(#routes_mod::#first_const, #chain)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract path parameter names from a route path string.
/// e.g., "users/{id}/posts/{post_id}" → ["id", "post_id"]
fn extract_path_param_names(path: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut chars = path.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '{' {
            let name: String = chars.by_ref().take_while(|&c| c != '}').collect();
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

/// Check if a type is `String` (bare, unqualified).
fn is_string_type(ty: &Type) -> bool {
    if let Type::Path(tp) = ty {
        if tp.qself.is_none() && tp.path.segments.len() == 1 {
            return tp.path.segments[0].ident == "String";
        }
    }
    false
}

/// Full routing function path for the first method in a chain.
/// e.g., `::axum::routing::get`
fn method_routing_fn(method: &HttpMethod) -> TokenStream {
    match method {
        HttpMethod::Get => quote! { ::axum::routing::get },
        HttpMethod::Post => quote! { ::axum::routing::post },
        HttpMethod::Put => quote! { ::axum::routing::put },
        HttpMethod::Delete => quote! { ::axum::routing::delete },
        HttpMethod::Patch => quote! { ::axum::routing::patch },
    }
}

/// Method name identifier for chaining on MethodRouter.
/// e.g., `.post(handler)`
fn method_name_ident(method: &HttpMethod) -> proc_macro2::Ident {
    format_ident!(
        "{}",
        match method {
            HttpMethod::Get => "get",
            HttpMethod::Post => "post",
            HttpMethod::Put => "put",
            HttpMethod::Delete => "delete",
            HttpMethod::Patch => "patch",
        }
    )
}

/// Qualify known framework types used in macro definitions.
///
/// Converts bare type names to fully qualified paths so that generated code
/// doesn't require the caller to have specific `use` statements in scope.
///
/// - `Json<T>` → `::axum::Json<T>`
/// - `Response` → `::axum::response::Response`
fn qualify_known_types(ty: &Type) -> TokenStream {
    if let Type::Path(tp) = ty {
        if tp.qself.is_none() && tp.path.segments.len() == 1 {
            let seg = &tp.path.segments[0];
            if seg.ident == "Json" {
                let args = &seg.arguments;
                return quote! { ::axum::Json #args };
            }
            if seg.ident == "Response" {
                return quote! { ::axum::response::Response };
            }
        }
    }
    // For any other type, emit as-is
    quote! { #ty }
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(ch.to_lowercase().next().unwrap());
        } else {
            result.push(ch);
        }
    }
    result
}